//! Channel module - Reliable bidirectional messaging
//!
//! Re-exports all channel types and implements the main Channel coordinator.

pub mod types;
pub mod state;

// Re-export common types
pub use types::*;
pub use state::{TxRing, RxRing, TxMessageEntry, RxMessageEntry};
