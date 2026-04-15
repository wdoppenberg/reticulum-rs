//! Monotonic clock and async delay abstractions
//!
//! The Reticulum protocol needs wall-clock time for retransmit timeouts,
//! keepalive intervals, and link-quality tracking.
//!
//! - Implement [`Clock`] to provide the current time.
//! - Implement [`Delay`] to provide async sleep — used by the runtime for
//!   retransmit backoffs and keepalive scheduling without busy-waiting.
//!
//! Both traits are typically implemented on the same adapter struct.
//!
//! # Requirements for [`Clock`]
//!
//! - The value returned by [`Clock::now_ms`] must be **monotonically
//!   non-decreasing** across calls.
//! - The epoch (t = 0) is arbitrary; only differences between timestamps are
//!   used by the protocol.
//! - Resolution should be at least 1 ms; coarser resolutions may cause
//!   premature retransmit timeouts.
//!
//! # Example (embassy)
//!
//! ```ignore
//! use embassy_time::{Instant, Timer, Duration};
//! use reticulum_core::clock::{Clock, Delay};
//!
//! pub struct EmbassyClock;
//!
//! impl Clock for EmbassyClock {
//!     fn now_ms(&self) -> u64 {
//!         Instant::now().as_millis()
//!     }
//! }
//!
//! impl Delay for EmbassyClock {
//!     async fn delay_ms(&mut self, ms: u64) {
//!         Timer::after(Duration::from_millis(ms)).await;
//!     }
//! }
//! ```
//!
//! # Example (tokio)
//!
//! ```ignore
//! use std::time::{Instant, Duration};
//! use reticulum_core::clock::{Clock, Delay};
//!
//! pub struct TokioClock { start: Instant }
//!
//! impl TokioClock {
//!     pub fn new() -> Self { Self { start: Instant::now() } }
//! }
//!
//! impl Clock for TokioClock {
//!     fn now_ms(&self) -> u64 {
//!         self.start.elapsed().as_millis() as u64
//!     }
//! }
//!
//! impl Delay for TokioClock {
//!     async fn delay_ms(&mut self, ms: u64) {
//!         tokio::time::sleep(Duration::from_millis(ms)).await;
//!     }
//! }
//! ```

/// A monotonic millisecond clock.
pub trait Clock {
    /// Returns the current time in milliseconds since an arbitrary epoch.
    ///
    /// Must be monotonically non-decreasing.
    fn now_ms(&self) -> u64;
}

/// A [`Clock`] that always returns zero.  Useful for tests and stubs where
/// real time is not needed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullClock;

impl Clock for NullClock {
    fn now_ms(&self) -> u64 {
        0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Delay
// ──────────────────────────────────────────────────────────────────────────────

/// Async delay abstraction.
///
/// Implement this for your target's timer source so that the Reticulum runtime
/// can sleep between retransmit attempts and keepalive ticks without
/// busy-waiting.
///
/// The same adapter struct can implement both [`Clock`] and [`Delay`].
///
/// The `async_fn_in_trait` lint is suppressed here intentionally: we do not
/// add `Send` bounds because Embassy tasks are `!Send` by design.
#[allow(async_fn_in_trait)]
pub trait Delay {
    /// Suspend execution for at least `ms` milliseconds.
    ///
    /// Implementations should yield to the executor (not spin) for the
    /// requested duration.
    async fn delay_ms(&mut self, ms: u64);
}

/// A [`Delay`] that returns immediately without sleeping.
///
/// Useful for unit tests where real delays would slow down the test suite.
/// In production, implement [`Delay`] for your platform's timer.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullDelay;

impl Delay for NullDelay {
    async fn delay_ms(&mut self, _ms: u64) {}
}
