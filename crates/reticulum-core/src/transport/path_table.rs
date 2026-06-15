//! Routing path table — maps destination hashes to next-hop addresses.
//!
//! # Feature flags
//!
//! | Feature    | Backing storage                         |
//! |------------|-----------------------------------------|
//! | `alloc`    | `BTreeMap` — heap, unbounded            |
//! | `heapless` | `FnvIndexMap<_, _, N>` — stack, const   |
//!
//! When both features are enabled `alloc` takes precedence.

#[cfg(feature = "alloc")]
use alloc::collections::BTreeMap;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::FnvIndexMap;

use crate::hash::{AddressHash, Hash};
use crate::packet::{DestinationType, Header, HeaderType, IfacFlag, Packet, PacketType};

#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct PathEntry {
    pub timestamp_ms: u64,
    pub received_from: AddressHash,
    pub hops: u8,
    pub iface: AddressHash,
    #[allow(dead_code)]
    pub packet_hash: Hash,
}

/// Routing path table.
///
/// `N` is the maximum number of entries (used only with the `heapless` feature;
/// must be a power of two).
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct PathTable<const N: usize = 128> {
    #[cfg(feature = "alloc")]
    map: BTreeMap<AddressHash, PathEntry>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    map: FnvIndexMap<AddressHash, PathEntry, N>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> PathTable<N> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            map: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            map: FnvIndexMap::new(),
        }
    }

    pub fn get(&self, destination: &AddressHash) -> Option<&PathEntry> {
        self.map.get(destination)
    }

    pub fn next_hop_full(&self, destination: &AddressHash) -> Option<(AddressHash, AddressHash)> {
        self.map.get(destination).map(|e| (e.received_from, e.iface))
    }

    #[allow(dead_code)]
    pub fn next_hop_iface(&self, destination: &AddressHash) -> Option<AddressHash> {
        self.map.get(destination).map(|e| e.iface)
    }

    #[allow(dead_code)]
    pub fn next_hop(&self, destination: &AddressHash) -> Option<AddressHash> {
        self.map.get(destination).map(|e| e.received_from)
    }

    pub fn handle_announce(
        &mut self,
        announce: &Packet,
        transport_id: Option<AddressHash>,
        iface: AddressHash,
        now_ms: u64,
    ) {
        let hops = announce.header.hops + 1;

        if let Some(existing) = self.map.get(&announce.destination) {
            if hops >= existing.hops {
                return;
            }
        }

        let received_from = transport_id.unwrap_or(announce.destination);
        let new_entry = PathEntry {
            timestamp_ms: now_ms,
            received_from,
            hops,
            iface,
            packet_hash: announce.hash(),
        };

        #[cfg(feature = "alloc")]
        {
            self.map.insert(announce.destination, new_entry);
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let _ = self.map.insert(announce.destination, new_entry);
        }

        log::info!(
            "{} is now reachable over {} hops through {}",
            announce.destination, hops, received_from,
        );
    }

    pub fn handle_inbound_packet(
        &self,
        original_packet: &Packet,
        lookup: Option<AddressHash>,
    ) -> (Packet, Option<AddressHash>) {
        let lookup = lookup.unwrap_or(original_packet.destination);
        let entry = match self.map.get(&lookup) {
            Some(e) => e,
            None => return (*original_packet, None),
        };
        (
            Packet {
                header: Header {
                    ifac_flag: IfacFlag::Authenticated,
                    header_type: HeaderType::Type2,
                    propagation_type: original_packet.header.propagation_type,
                    destination_type: original_packet.header.destination_type,
                    packet_type: original_packet.header.packet_type,
                    hops: original_packet.header.hops + 1,
                },
                ifac: None,
                destination: original_packet.destination,
                transport: Some(entry.received_from),
                context: original_packet.context,
                data: original_packet.data,
            },
            Some(entry.iface),
        )
    }

    #[allow(dead_code)]
    pub fn refresh(&mut self, destination: &AddressHash, now_ms: u64) {
        if let Some(entry) = self.map.get_mut(destination) {
            entry.timestamp_ms = now_ms;
        }
    }

    pub fn handle_packet(&mut self, original_packet: &Packet) -> (Packet, Option<AddressHash>) {
        if original_packet.header.header_type == HeaderType::Type2
            || original_packet.header.packet_type == PacketType::Announce
            || original_packet.header.destination_type == DestinationType::Plain
            || original_packet.header.destination_type == DestinationType::Group
        {
            return (*original_packet, None);
        }

        let entry = match self.map.get(&original_packet.destination) {
            Some(e) => e,
            None => return (*original_packet, None),
        };

        (
            Packet {
                header: Header {
                    ifac_flag: IfacFlag::Authenticated,
                    header_type: HeaderType::Type2,
                    propagation_type: original_packet.header.propagation_type,
                    destination_type: original_packet.header.destination_type,
                    packet_type: original_packet.header.packet_type,
                    hops: original_packet.header.hops,
                },
                ifac: original_packet.ifac,
                destination: original_packet.destination,
                transport: Some(entry.received_from),
                context: original_packet.context,
                data: original_packet.data,
            },
            Some(entry.iface),
        )
    }
}
