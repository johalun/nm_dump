extern crate libc;
extern crate netmap;
extern crate byteorder;

use std::env;
use std::mem;
use std::fmt;

use netmap::*;
use byteorder::{ReadBytesExt, WriteBytesExt, BigEndian, LittleEndian};

struct MacAddress(u8, u8, u8, u8, u8, u8);
impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
               "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
               self.0,
               self.1,
               self.2,
               self.3,
               self.4,
               self.5)
    }
}

#[repr(packed)]
struct MacHeader {
    dst: MacAddress,
    src: MacAddress,
    ethertype: u16, // Need endian conversion to display
}
impl MacHeader {
    fn ethertype(&self) -> u16 {
        (self.ethertype & 0xFF) << 8 | (self.ethertype & 0xFF00) >> 8
    }
    fn ethertype_string(&self) -> &str {
        let v = (self.ethertype & 0xFF) << 8 | (self.ethertype & 0xFF00) >> 8;
        match v {
            0x0800 => "IPv4",
            _ => "Unknown",
        }
    }
}
impl fmt::Display for MacHeader {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
               "Type {}. {} -> {}",
               self.ethertype_string(),
               self.src,
               self.dst)
    }
}

struct IPv4Address(u8, u8, u8, u8);
impl fmt::Display for IPv4Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0, self.1, self.2, self.3)
    }
}

#[repr(packed)]
struct IPv4Header {
    padding1: [u8; 12],
    src: IPv4Address,
    dst: IPv4Address,
    padding2: [u8; 16],
}
impl fmt::Display for IPv4Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} -> {}", self.src, self.dst)
    }
}

fn move_packets(src: &mut netmap::NetmapDescriptor, dst: &mut netmap::NetmapDescriptor) {
    {
        let mut rx_slots = src.rx_iter().flat_map(|rx_ring| rx_ring.iter());
        'rings: for tx_ring in dst.tx_iter() {
            println!("===> TX ring");
            let mut tx_slot_iter = tx_ring.iter_mut();
            'slots: loop {
                println!("====> TX slot");
                match tx_slot_iter.next() {
                    None => break 'slots,
                    Some(tx_slot_buf) => {
                        match rx_slots.next() {
                            None => {
                                println!("====> End of rx queue");
                                tx_slot_iter.give_back();
                                break 'rings;
                            }
                            Some(rx_slot_buf) => {
                                let packet_size: u16 = rx_slot_buf.0.get_len();
                                println!("====> Copy buffer from rx slot to tx slot. Size {} \
                                          bytes.",
                                         packet_size);
                                let mac_size = 14;
                                let mac_slice = &rx_slot_buf.1[0..mac_size];
                                let mac_ptr = mac_slice.as_ptr();
                                let mac: &MacHeader = unsafe { mem::transmute(mac_ptr) };
                                println!("====> MAC Header: {}", mac);

                                let mut offset = mac_size;
                                if mac.ethertype() == 0x0800 {
                                    let ipv4_size = 36;
                                    offset += ipv4_size as usize;
                                    let ipv4_slice: &[u8] =
                                        &rx_slot_buf.1[mac_size..ipv4_size as usize];
                                    let ipv4_ptr = ipv4_slice.as_ptr();
                                    let ipv4: &IPv4Header = unsafe { mem::transmute(ipv4_ptr) };
                                    println!("====> IPv4 Header: {}", ipv4);
                                }
                                let payload_slice: &[u8] = &rx_slot_buf.1[offset..];
                                println!("====> Payload: {:?}", payload_slice);

                                let tgt_buf = &mut tx_slot_buf.1[0..packet_size as usize];
                                tgt_buf.copy_from_slice(rx_slot_buf.1);
                                tx_slot_buf.0.set_len(packet_size);
                            }
                        }
                    }
                }
            }
        }
    }

    // Tell the kernel it can use the slots we've consumed.
    for ring in src.rx_iter() {
        println!("==> Set head in src ring");
        ring.head_from_cur();
    }

    // Tell the kernel it can send the slots we've populated.
    for ring in dst.tx_iter() {
        println!("==> Set head in dst ring");
        ring.head_from_cur();
    }
}

fn main() {
    let iface = env::args().nth(1).expect("expected interface name as argument (e.g. eth0)");
    let mut nm_wire = NetmapDescriptor::new(&iface).expect("can't open netmap for interface");
    let mut nm_host = NetmapDescriptor::new(&(iface + "^"))
        .expect("can't open netmap host interface");
    let mut pollfds: Vec<libc::pollfd> = Vec::with_capacity(2);
    pollfds.push(libc::pollfd {
        fd: nm_wire.get_fd(),
        events: 0,
        revents: 0,
    });
    pollfds.push(libc::pollfd {
        fd: nm_host.get_fd(),
        events: 0,
        revents: 0,
    });
    loop {
        println!("============================================>");
        for pollfd in pollfds.iter_mut() {
            pollfd.events = libc::POLLIN;
            pollfd.revents = 0;
        }
        if let Some(first) = pollfds.first_mut() {
            match unsafe { libc::poll(first as *mut libc::pollfd, 2, 2000) } {
                x if x < 0 => panic!("poll failure"),
                x if x == 0 => {
                    println!("=> Poll timeout");
                    continue;
                }
                _ => (),
            }
        } else {
            panic!("no fd vec?")
        }
        // A netmap poll error can mean the rings get reset: loop again.
        for pollfd in pollfds.iter() {
            if pollfd.revents & libc::POLLERR == libc::POLLERR {
                println!("=> Poll error");
                continue;
            }
        }

        // Handle outgoing packets
        println!("=> Move from host to wire");
        move_packets(&mut nm_host, &mut nm_wire);

        println!("");

        // Handle incoming packets
        println!("=> Move from wire to host");
        move_packets(&mut nm_wire, &mut nm_host);
    }
}
