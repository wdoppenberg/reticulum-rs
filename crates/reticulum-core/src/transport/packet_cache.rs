//! Packet deduplication cache.
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

use core::cmp::min;

use crate::hash::Hash;
use crate::packet::Packet;

#[cfg(any(feature = "alloc", feature = "heapless"))]
#[derive(Debug, Clone)]
pub struct PacketTrack {
    pub time_ms: u64,
    pub min_hops: u8,
}

/// Packet deduplication cache.
///
/// `N` is the maximum number of entries (used only with the `heapless` feature;
/// must be a power of two).  Under `alloc` the map grows unboundedly.
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct PacketCache<const N: usize = 256> {
    #[cfg(feature = "alloc")]
    map: BTreeMap<Hash, PacketTrack>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    map: FnvIndexMap<Hash, PacketTrack, N>,

    /// Scratch buffer for keys to remove (alloc only).
    #[cfg(feature = "alloc")]
    remove_cache: Vec<Hash>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> PacketCache<N> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            map: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            map: FnvIndexMap::new(),
            #[cfg(feature = "alloc")]
            remove_cache: Vec::new(),
        }
    }

    /// Remove entries whose age exceeds `max_age_ms`.
    pub fn release(&mut self, max_age_ms: u64, now_ms: u64) {
        #[cfg(feature = "alloc")]
        {
            for (hash, track) in &self.map {
                if now_ms.saturating_sub(track.time_ms) > max_age_ms {
                    self.remove_cache.push(*hash);
                }
            }
            for hash in &self.remove_cache {
                self.map.remove(hash);
            }
            self.remove_cache.clear();
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            // Collect stale keys into a stack-allocated scratch buffer.
            let mut stale: heapless::Vec<Hash, N> = heapless::Vec::new();
            for (hash, track) in &self.map {
                if now_ms.saturating_sub(track.time_ms) > max_age_ms {
                    let _ = stale.push(*hash);
                }
            }
            for hash in &stale {
                self.map.remove(hash);
            }
        }
    }

    /// Returns `true` if this packet has not been seen before.
    pub fn update(&mut self, packet: &Packet, now_ms: u64) -> bool {
        let hash = packet.hash();
        if let Some(track) = self.map.get_mut(&hash) {
            track.time_ms = now_ms;
            track.min_hops = min(packet.header.hops, track.min_hops);
            false
        } else {
            #[cfg(feature = "alloc")]
            {
                self.map.insert(hash, PacketTrack { time_ms: now_ms, min_hops: packet.header.hops });
            }
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            {
                // Ignore insertion errors when full — a full cache just allows
                // some duplicates through, which is safe.
                let _ = self.map.insert(hash, PacketTrack { time_ms: now_ms, min_hops: packet.header.hops });
            }
            true
        }
    }
}
