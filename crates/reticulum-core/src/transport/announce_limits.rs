//! Per-source announce rate limiter.
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

use crate::hash::AddressHash;

#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct AnnounceRateLimit {
    pub target_ms: u64,
    pub grace: u32,
    pub penalty_ms: Option<u64>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl Default for AnnounceRateLimit {
    fn default() -> Self {
        Self { target_ms: 3_600_000, grace: 10, penalty_ms: Some(7_200_000) }
    }
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
struct AnnounceLimitEntry {
    rate_limit: Option<AnnounceRateLimit>,
    violations: u32,
    last_announce_ms: u64,
    blocked_until_ms: u64,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl AnnounceLimitEntry {
    fn new(rate_limit: Option<AnnounceRateLimit>, now_ms: u64) -> Self {
        Self { rate_limit, violations: 0, last_announce_ms: now_ms, blocked_until_ms: now_ms }
    }

    fn handle_announce(&mut self, now_ms: u64) -> Option<u64> {
        let mut is_blocked = false;

        if let Some(ref rate_limit) = self.rate_limit {
            if now_ms < self.blocked_until_ms {
                self.blocked_until_ms = now_ms + rate_limit.target_ms;
                if let Some(penalty_ms) = rate_limit.penalty_ms {
                    self.blocked_until_ms += penalty_ms;
                }
                is_blocked = true;
            } else {
                let next_allowed_ms = self.last_announce_ms + rate_limit.target_ms;
                if now_ms < next_allowed_ms {
                    self.violations += 1;
                    if self.violations >= rate_limit.grace {
                        self.violations = 0;
                        self.blocked_until_ms = now_ms + rate_limit.target_ms;
                        is_blocked = true;
                    }
                }
            }
        }

        self.last_announce_ms = now_ms;
        if is_blocked { Some(self.blocked_until_ms - now_ms) } else { None }
    }
}

/// Per-source announce rate limiter.
///
/// `N` is the maximum number of tracked sources (used only with the `heapless`
/// feature; must be a power of two).
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct AnnounceLimits<const N: usize = 64> {
    #[cfg(feature = "alloc")]
    limits: BTreeMap<AddressHash, AnnounceLimitEntry>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    limits: FnvIndexMap<AddressHash, AnnounceLimitEntry, N>,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> AnnounceLimits<N> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            limits: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            limits: FnvIndexMap::new(),
        }
    }

    /// Returns `Some(ms_until_unblocked)` if this announce should be dropped.
    pub fn check(&mut self, destination: &AddressHash, now_ms: u64) -> Option<u64> {
        if let Some(entry) = self.limits.get_mut(destination) {
            return entry.handle_announce(now_ms);
        }
        #[cfg(feature = "alloc")]
        {
            self.limits.insert(
                *destination,
                AnnounceLimitEntry::new(Some(AnnounceRateLimit::default()), now_ms),
            );
        }
        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        {
            let _ = self.limits.insert(
                *destination,
                AnnounceLimitEntry::new(Some(AnnounceRateLimit::default()), now_ms),
            );
        }
        None
    }
}
