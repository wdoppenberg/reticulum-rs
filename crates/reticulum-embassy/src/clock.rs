//! Embassy-backed [`Clock`] and [`Delay`] implementations.
//!
//! [`EmbassyClock`] is a zero-sized adapter that delegates to
//! [`embassy_time::Instant`] for the current time and
//! [`embassy_time::Timer`] for async delays.
//!
//! # Usage
//!
//! ```rust,ignore
//! use reticulum_embassy::clock::EmbassyClock;
//! use reticulum_core::clock::{Clock, Delay};
//!
//! let mut clk = EmbassyClock;
//! let now = clk.now_ms();
//! clk.delay_ms(100).await;
//! ```
//!
//! [`Clock`]: reticulum_core::clock::Clock
//! [`Delay`]: reticulum_core::clock::Delay

use embassy_time::{Duration, Instant, Timer};
use reticulum_core::clock::{Clock, Delay};

/// A zero-sized [`Clock`] + [`Delay`] adapter backed by `embassy_time`.
///
/// The epoch is the first call to [`Instant::now()`] after the executor
/// starts, which is implementation-defined but always monotonic.
///
/// Copy freely — it holds no state.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbassyClock;

impl Clock for EmbassyClock {
    /// Returns milliseconds elapsed since an arbitrary (but stable) epoch.
    ///
    /// Backed by [`embassy_time::Instant::now().as_millis()`].
    fn now_ms(&self) -> u64 {
        Instant::now().as_millis()
    }
}

impl Delay for EmbassyClock {
    /// Sleeps for at least `ms` milliseconds by yielding to the executor.
    async fn delay_ms(&mut self, ms: u64) {
        Timer::after(Duration::from_millis(ms)).await;
    }
}
