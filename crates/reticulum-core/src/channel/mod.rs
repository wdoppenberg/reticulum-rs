//! Channel module - Reliable bidirectional messaging
//!
//! Re-exports all channel types and implements the main Channel coordinator.

pub mod state;
pub mod types;

// Re-export common types
pub use types::{
    calculate_timeout, Envelope, LinkSpeed, MessageState, MessageType, SequenceNumber,
    SystemMessageType, ENVELOPE_HEADER_SIZE, MAX_ENVELOPE_SIZE, MAX_RETRIES, MIN_RTT_MS, SEQ_MAX,
    SEQ_MODULUS, WINDOW_FLEXIBILITY, WINDOW_INITIAL, WINDOW_MAX_FAST, WINDOW_MAX_MEDIUM,
    WINDOW_MAX_SLOW, WINDOW_MIN, WINDOW_MIN_LIMIT_FAST, WINDOW_MIN_LIMIT_MEDIUM,
    WINDOW_MIN_LIMIT_SLOW,
};

#[cfg(any(feature = "alloc", feature = "heapless"))]
pub use state::{RxMessageEntry, RxRing, TxMessageEntry, TxRing};
