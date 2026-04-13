//! Channel module - Reliable bidirectional messaging
//!
//! Re-exports all channel types and implements the main Channel coordinator.

pub mod state;
pub mod types;

// Re-export common types
pub use state::{RxMessageEntry, RxRing, TxMessageEntry, TxRing};
pub use types::*;
