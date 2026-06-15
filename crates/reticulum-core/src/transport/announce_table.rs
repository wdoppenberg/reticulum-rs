//! Announce retransmission table.
//!
//! # Feature flags
//!
//! | Feature    | Backing storage                                       |
//! |------------|-------------------------------------------------------|
//! | `alloc`    | `BTreeMap` — heap, unbounded; includes overflow cache |
//! | `heapless` | `FnvIndexMap<_, _, N>` — stack, const; no overflow cache |
//!
//! When both features are enabled `alloc` takes precedence.
//!
//! ## Allocation-free drain
//!
//! For `heapless` targets use [`AnnounceTable::drain_retransmits_with`], which
//! drives a caller-supplied closure for each retransmit packet.  Under `alloc`
//! the convenience [`AnnounceTable::drain_retransmits`] collects into a `Vec`.

#[cfg(feature = "alloc")]
use alloc::collections::BTreeMap;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::FnvIndexMap;

use crate::hash::AddressHash;
use crate::packet::{
    DestinationType, Header, HeaderType, IfacFlag, Packet, PacketContext, PacketType,
    PropagationType,
};
use crate::routing::{TxMessage, TxMessageType};

#[cfg(any(feature = "alloc", feature = "heapless"))]
#[derive(Clone)]
pub struct AnnounceEntry {
    pub packet: Packet,
    pub timestamp_ms: u64,
    pub timeout_ms: u64,
    pub received_from: AddressHash,
    pub retries: u8,
    pub hops: u8,
    pub response_to_iface: Option<AddressHash>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl AnnounceEntry {
    pub fn retransmit(&mut self, transport_id: &AddressHash, now_ms: u64) -> Option<TxMessage> {
        if self.retries == 0 || now_ms >= self.timeout_ms {
            return None;
        }
        self.retries = self.retries.saturating_sub(1);

        let context = if self.response_to_iface.is_some() {
            PacketContext::PathResponse
        } else {
            PacketContext::None
        };

        let packet = Packet {
            header: Header {
                ifac_flag: IfacFlag::Open,
                header_type: HeaderType::Type2,
                propagation_type: PropagationType::Broadcast,
                destination_type: DestinationType::Single,
                packet_type: PacketType::Announce,
                hops: self.hops,
            },
            ifac: None,
            destination: self.packet.destination,
            transport: Some(*transport_id),
            context,
            data: self.packet.data,
        };

        let tx_type = match self.response_to_iface {
            Some(iface) => TxMessageType::Direct(iface),
            None => TxMessageType::Broadcast(Some(self.received_from)),
        };

        Some(TxMessage { tx_type, packet })
    }
}

// ── Overflow cache (alloc only) ───────────────────────────────────────────────

#[cfg(feature = "alloc")]
struct AnnounceCache {
    newer: Option<BTreeMap<AddressHash, AnnounceEntry>>,
    older: Option<BTreeMap<AddressHash, AnnounceEntry>>,
    capacity: usize,
}

#[cfg(feature = "alloc")]
impl AnnounceCache {
    fn new(capacity: usize) -> Self {
        Self { newer: Some(BTreeMap::new()), older: None, capacity }
    }

    fn insert(&mut self, destination: AddressHash, entry: AnnounceEntry) {
        if self.newer.as_ref().unwrap().len() >= self.capacity {
            self.older = Some(self.newer.take().unwrap());
            self.newer = Some(BTreeMap::new());
        }
        self.newer.as_mut().unwrap().insert(destination, entry);
    }

    fn get(&self, destination: &AddressHash) -> Option<AnnounceEntry> {
        self.newer.as_ref().unwrap().get(destination).cloned()
            .or_else(|| self.older.as_ref().and_then(|m| m.get(destination).cloned()))
    }

    fn clear(&mut self) {
        self.newer.as_mut().unwrap().clear();
        self.older = None;
    }
}

// ── AnnounceTable ─────────────────────────────────────────────────────────────

/// Announce retransmission table.
///
/// `N` is the maximum number of pending announce entries (used only with the
/// `heapless` feature; must be a power of two).  Under `alloc` the map grows
/// unboundedly and includes a two-generation overflow cache.
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct AnnounceTable<const N: usize = 256> {
    #[cfg(feature = "alloc")]
    map: BTreeMap<AddressHash, AnnounceEntry>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    map: FnvIndexMap<AddressHash, AnnounceEntry, N>,

    #[cfg(feature = "alloc")]
    responses: BTreeMap<AddressHash, AnnounceEntry>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    responses: FnvIndexMap<AddressHash, AnnounceEntry, N>,

    /// Overflow cache only present with `alloc`.
    #[cfg(feature = "alloc")]
    cache: AnnounceCache,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> AnnounceTable<N> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            map: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            map: FnvIndexMap::new(),

            #[cfg(feature = "alloc")]
            responses: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            responses: FnvIndexMap::new(),

            #[cfg(feature = "alloc")]
            cache: AnnounceCache::new(100_000),
        }
    }

    pub fn add(
        &mut self,
        announce: &Packet,
        destination: AddressHash,
        received_from: AddressHash,
        now_ms: u64,
    ) {
        if self.map.contains_key(&destination) {
            return;
        }
        let entry = AnnounceEntry {
            packet: *announce,
            timestamp_ms: now_ms,
            timeout_ms: now_ms + 60_000,
            received_from,
            retries: 5,
            hops: announce.header.hops + 1,
            response_to_iface: None,
        };
        #[cfg(feature = "alloc")]
        {
            self.map.insert(destination, entry);
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let _ = self.map.insert(destination, entry);
        }
    }

    fn do_add_response(
        &mut self,
        mut response: AnnounceEntry,
        destination: AddressHash,
        to_iface: AddressHash,
        hops: u8,
        now_ms: u64,
    ) {
        response.retries = 1;
        response.hops = hops;
        response.timeout_ms = now_ms + 60_000;
        response.response_to_iface = Some(to_iface);
        #[cfg(feature = "alloc")]
        {
            self.responses.insert(destination, response);
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let _ = self.responses.insert(destination, response);
        }
    }

    pub fn add_response(
        &mut self,
        destination: AddressHash,
        to_iface: AddressHash,
        hops: u8,
        now_ms: u64,
    ) -> bool {
        if let Some(entry) = self.map.get(&destination) {
            self.do_add_response(entry.clone(), destination, to_iface, hops, now_ms);
            return true;
        }
        #[cfg(feature = "alloc")]
        if let Some(entry) = self.cache.get(&destination) {
            self.do_add_response(entry, destination, to_iface, hops, now_ms);
            return true;
        }
        false
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.map.clear();
        self.responses.clear();
        #[cfg(feature = "alloc")]
        self.cache.clear();
    }

    pub fn new_packet(
        &mut self,
        dest_hash: &AddressHash,
        transport_id: &AddressHash,
        now_ms: u64,
    ) -> Option<TxMessage> {
        self.map.get_mut(dest_hash).and_then(|e| e.retransmit(transport_id, now_ms))
    }

    /// Drain pending retransmits, calling `f` for each `TxMessage`.
    ///
    /// This is the allocation-free variant.  For `alloc` convenience use
    /// [`drain_retransmits`](Self::drain_retransmits).
    pub fn drain_retransmits_with<F: FnMut(TxMessage)>(
        &mut self,
        transport_id: &AddressHash,
        now_ms: u64,
        mut f: F,
    ) {
        #[cfg(feature = "alloc")]
        let mut completed = Vec::new();
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        let mut completed: heapless::Vec<AddressHash, N> = heapless::Vec::new();

        let mut n_announces = 0usize;
        let mut n_responses = 0usize;

        for (destination, entry) in &mut self.map {
            if self.responses.contains_key(destination) {
                continue;
            }
            if let Some(message) = entry.retransmit(transport_id, now_ms) {
                n_announces += 1;
                f(message);
            } else {
                #[cfg(feature = "alloc")]
                completed.push(*destination);
                #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
                let _ = completed.push(*destination);
            }
        }

        for entry in self.responses.values_mut() {
            if let Some(message) = entry.retransmit(transport_id, now_ms) {
                n_responses += 1;
                f(message);
            }
        }

        self.responses.clear();

        if n_announces + n_responses > 0 || !completed.is_empty() {
            log::trace!(
                "Announce cache: {} retransmitted, {} path responses, {} dropped",
                n_announces, n_responses, completed.len(),
            );
        }

        for destination in &completed {
            #[cfg(feature = "alloc")]
            if let Some(announce) = self.map.remove(destination) {
                self.cache.insert(*destination, announce);
            }
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            {
                self.map.remove(destination);
            }
        }
    }

    /// Drain pending retransmits into a `Vec`.
    ///
    /// Requires the `alloc` feature.  For `no_alloc` use
    /// [`drain_retransmits_with`](Self::drain_retransmits_with).
    #[cfg(feature = "alloc")]
    pub fn drain_retransmits(
        &mut self,
        transport_id: &AddressHash,
        now_ms: u64,
    ) -> Vec<TxMessage> {
        let mut messages = Vec::new();
        self.drain_retransmits_with(transport_id, now_ms, |m| messages.push(m));
        messages
    }
}
