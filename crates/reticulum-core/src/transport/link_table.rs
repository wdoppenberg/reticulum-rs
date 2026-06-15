//! Relay link table — tracks in-flight link requests forwarded as an
//! intermediate node.
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
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::FnvIndexMap;

use crate::hash::AddressHash;
use crate::link::LinkId;
use crate::packet::{Header, HeaderType, IfacFlag, Packet};

#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct LinkEntry {
    pub timestamp_ms: u64,
    pub proof_timeout_ms: u64,
    pub next_hop: AddressHash,
    #[allow(dead_code)]
    pub next_hop_iface: AddressHash,
    pub received_from: AddressHash,
    pub original_destination: AddressHash,
    #[allow(dead_code)]
    pub taken_hops: u8,
    pub remaining_hops: u8,
    pub validated: bool,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
fn send_backwards(packet: &Packet, entry: &LinkEntry) -> (Packet, AddressHash) {
    (
        Packet {
            header: Header {
                ifac_flag: IfacFlag::Authenticated,
                header_type: HeaderType::Type2,
                propagation_type: packet.header.propagation_type,
                destination_type: packet.header.destination_type,
                packet_type: packet.header.packet_type,
                hops: packet.header.hops + 1,
            },
            ifac: None,
            destination: packet.destination,
            transport: Some(entry.next_hop),
            context: packet.context,
            data: packet.data,
        },
        entry.received_from,
    )
}

/// Relay link table.
///
/// `N` is the maximum number of entries (used only with the `heapless` feature;
/// must be a power of two).
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct LinkTable<const N: usize = 64> {
    #[cfg(feature = "alloc")]
    map: BTreeMap<LinkId, LinkEntry>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    map: FnvIndexMap<LinkId, LinkEntry, N>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> LinkTable<N> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            map: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            map: FnvIndexMap::new(),
        }
    }

    pub fn add(
        &mut self,
        link_request: &Packet,
        destination: AddressHash,
        received_from: AddressHash,
        next_hop: AddressHash,
        iface: AddressHash,
        now_ms: u64,
    ) {
        let link_id = LinkId::from(link_request);
        if self.map.contains_key(&link_id) {
            return;
        }
        let entry = LinkEntry {
            timestamp_ms: now_ms,
            proof_timeout_ms: now_ms + 600_000,
            next_hop,
            next_hop_iface: iface,
            received_from,
            original_destination: destination,
            taken_hops: link_request.header.hops + 1,
            remaining_hops: 0,
            validated: false,
        };
        #[cfg(feature = "alloc")]
        {
            self.map.insert(link_id, entry);
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let _ = self.map.insert(link_id, entry);
        }
    }

    pub fn original_destination(&self, link_id: &LinkId) -> Option<AddressHash> {
        self.map.get(link_id).filter(|e| e.validated).map(|e| e.original_destination)
    }

    pub fn handle_keepalive(&self, packet: &Packet) -> Option<(Packet, AddressHash)> {
        self.map.get(&packet.destination).map(|e| send_backwards(packet, e))
    }

    pub fn handle_proof(&mut self, proof: &Packet) -> Option<(Packet, AddressHash)> {
        match self.map.get_mut(&proof.destination) {
            Some(entry) => {
                entry.remaining_hops = proof.header.hops;
                entry.validated = true;
                Some(send_backwards(proof, entry))
            }
            None => None,
        }
    }

    const ACTIVE_RELAY_TIMEOUT_MS: u64 = 600_000;

    pub fn remove_stale(&mut self, now_ms: u64) {
        #[cfg(feature = "alloc")]
        {
            let mut stale = Vec::new();
            for (link_id, entry) in &self.map {
                if entry.validated {
                    if entry.timestamp_ms + Self::ACTIVE_RELAY_TIMEOUT_MS <= now_ms {
                        stale.push(*link_id);
                    }
                } else if entry.proof_timeout_ms <= now_ms {
                    stale.push(*link_id);
                }
            }
            for link_id in stale {
                self.map.remove(&link_id);
            }
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let mut stale: heapless::Vec<LinkId, N> = heapless::Vec::new();
            for (link_id, entry) in &self.map {
                let is_stale = if entry.validated {
                    entry.timestamp_ms + Self::ACTIVE_RELAY_TIMEOUT_MS <= now_ms
                } else {
                    entry.proof_timeout_ms <= now_ms
                };
                if is_stale {
                    let _ = stale.push(*link_id);
                }
            }
            for link_id in &stale {
                self.map.remove(link_id);
            }
        }
    }
}
