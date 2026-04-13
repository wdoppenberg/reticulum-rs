//! Channel - Reliable bidirectional messaging over Links
//!
//! The Channel system provides a reliable, ordered message delivery protocol
//! on top of Reticulum links using a sliding window protocol with automatic
//! retransmission and adaptive window sizing.
//!
//! # Overview
//!
//! - **Reliable Delivery**: Messages are guaranteed to be delivered in order
//! - **Sliding Window**: Dynamic window size (2-48 packets) adapts to link performance
//! - **Sequence Tracking**: 16-bit sequence numbers with wraparound handling
//! - **Automatic Retry**: Up to 5 retries with exponential backoff
//! - **Message Types**: Support for multiple registered message types
//!
//! # Protocol
//!
//! ## Message Envelope Format (6 bytes header + payload)
//!
//! ```text
//! ┌─────────────┬─────────────┬──────────────┬─────────────┐
//! │ Msg Type    │ Sequence    │ Length       │ Payload     │
//! │ (2 bytes)   │ (2 bytes)   │ (2 bytes)    │ (variable)  │
//! └─────────────┴─────────────┴──────────────┴─────────────┘
//! ```
//!
//! ## Window Sizing
//!
//! - **Slow Links** (< 1 Kbps): Window 2-5
//! - **Medium Links** (1-100 Kbps): Window 5-12
//! - **Fast Links** (> 100 Kbps): Window 16-48
//!
//! ## Timeout Calculation
//!
//! ```text
//! timeout = (1.5 ^ (tries - 1)) × max(RTT × 2.5, 25ms) × (tx_ring_size + 1.5)
//! ```

#![cfg_attr(not(feature = "alloc"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

use core::fmt;
use crate::error::RnsError;
use crate::buffer::StaticBuffer;

/// Maximum sequence number (16-bit)
pub const SEQ_MAX: u16 = 0xFFFF;

/// Sequence modulus for wraparound
pub const SEQ_MODULUS: u32 = (SEQ_MAX as u32) + 1;

/// Envelope header size (msg_type + sequence + length)
pub const ENVELOPE_HEADER_SIZE: usize = 6;

/// Maximum envelope size (for static buffer allocation)
pub const MAX_ENVELOPE_SIZE: usize = 2048;

/// Initial window size
pub const WINDOW_INITIAL: usize = 2;

/// Minimum window size (absolute minimum)
pub const WINDOW_MIN: usize = 2;

/// Window size limits by link speed
pub const WINDOW_MIN_LIMIT_SLOW: usize = 2;
pub const WINDOW_MIN_LIMIT_MEDIUM: usize = 5;
pub const WINDOW_MIN_LIMIT_FAST: usize = 16;

pub const WINDOW_MAX_SLOW: usize = 5;
pub const WINDOW_MAX_MEDIUM: usize = 12;
pub const WINDOW_MAX_FAST: usize = 48;

/// Window flexibility factor
pub const WINDOW_FLEXIBILITY: usize = 4;

/// Maximum retry attempts
pub const MAX_RETRIES: u8 = 5;

/// Minimum RTT for timeout calculation (25ms)
pub const MIN_RTT_MS: u32 = 25;

/// Message state in the transmission lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageState {
    /// Message created but not sent
    New,
    /// Message sent, awaiting acknowledgment
    Sent,
    /// Message acknowledged and delivered
    Delivered,
    /// Message failed after retries
    Failed,
}

/// Message type identifier (16-bit)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MessageType(pub u16);

impl MessageType {
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }
}

/// System message types (reserved range)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SystemMessageType {
    /// Keepalive message
    Keepalive = 0xF000,
    /// Window size advertisement
    WindowSize = 0xF001,
}

impl From<SystemMessageType> for MessageType {
    fn from(smt: SystemMessageType) -> Self {
        MessageType(smt as u16)
    }
}

/// Sequence number (16-bit with wraparound)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceNumber(u16);

impl SequenceNumber {
    pub const fn new(seq: u16) -> Self {
        Self(seq)
    }

    pub const fn zero() -> Self {
        Self(0)
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    /// Increment sequence number with wraparound
    pub fn increment(&mut self) {
        self.0 = ((self.0 as u32 + 1) % SEQ_MODULUS) as u16;
    }

    /// Get next sequence number
    pub fn next(&self) -> Self {
        Self((((self.0 as u32) + 1) % SEQ_MODULUS) as u16)
    }

    /// Calculate distance between sequence numbers (handling wraparound)
    pub fn distance_to(&self, other: SequenceNumber) -> u16 {
        if other.0 >= self.0 {
            other.0 - self.0
        } else {
            // Wrapped around
            (SEQ_MAX - self.0) + other.0 + 1
        }
    }

    /// Check if this sequence is before another (handling wraparound)
    pub fn is_before(&self, other: SequenceNumber) -> bool {
        let distance = self.distance_to(other);
        distance > 0 && distance < (SEQ_MAX / 2)
    }
}

impl fmt::Display for SequenceNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Message envelope containing type, sequence, and payload
///
/// Wire format:
/// ```text
/// | msg_type (2) | sequence (2) | length (2) | payload (n) |
/// ```
#[derive(Debug, Clone)]
pub struct Envelope<const N: usize = MAX_ENVELOPE_SIZE> {
    pub msg_type: MessageType,
    pub sequence: SequenceNumber,
    pub payload: StaticBuffer<N>,
}

impl<const N: usize> Envelope<N> {
    /// Compile-time guard: the buffer must be large enough to hold the 6-byte
    /// header plus at least one byte of payload.
    const _MIN_SIZE: () = assert!(
        N >= ENVELOPE_HEADER_SIZE,
        "Envelope buffer N must be at least ENVELOPE_HEADER_SIZE (6) bytes"
    );

    /// Create a new envelope
    pub fn new(msg_type: MessageType, sequence: SequenceNumber, payload: &[u8]) -> Result<Self, RnsError> {
        let mut payload_buf = StaticBuffer::new();
        payload_buf.write(payload)?;

        Ok(Self {
            msg_type,
            sequence,
            payload: payload_buf,
        })
    }

    /// Pack envelope into wire format
    pub fn pack(&self) -> StaticBuffer<N> {
        let mut buffer = StaticBuffer::new();

        // Message type (2 bytes, big-endian)
        buffer.safe_write(&self.msg_type.0.to_be_bytes());

        // Sequence number (2 bytes, big-endian)
        buffer.safe_write(&self.sequence.0.to_be_bytes());

        // Payload length (2 bytes, big-endian)
        let payload_len = self.payload.len() as u16;
        buffer.safe_write(&payload_len.to_be_bytes());

        // Payload
        buffer.safe_write(self.payload.as_slice());

        buffer
    }

    /// Unpack envelope from wire format
    pub fn unpack(data: &[u8]) -> Result<Self, RnsError> {
        if data.len() < ENVELOPE_HEADER_SIZE {
            return Err(RnsError::InvalidArgument);
        }

        // Parse message type
        let msg_type = u16::from_be_bytes([data[0], data[1]]);
        let msg_type = MessageType::new(msg_type);

        // Parse sequence number
        let sequence = u16::from_be_bytes([data[2], data[3]]);
        let sequence = SequenceNumber::new(sequence);

        // Parse payload length
        let payload_len = u16::from_be_bytes([data[4], data[5]]) as usize;

        // Validate payload length
        if data.len() < ENVELOPE_HEADER_SIZE + payload_len {
            return Err(RnsError::InvalidArgument);
        }

        // Extract payload
        let payload = &data[ENVELOPE_HEADER_SIZE..(ENVELOPE_HEADER_SIZE + payload_len)];

        Self::new(msg_type, sequence, payload)
    }

    /// Get payload as slice
    pub fn payload_slice(&self) -> &[u8] {
        self.payload.as_slice()
    }

    /// Get envelope size (header + payload)
    pub fn size(&self) -> usize {
        ENVELOPE_HEADER_SIZE + self.payload.len()
    }
}

/// Link speed classification for window sizing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSpeed {
    /// < 1 Kbps
    Slow,
    /// 1-100 Kbps
    Medium,
    /// > 100 Kbps
    Fast,
}

impl LinkSpeed {
    /// Classify link speed from bitrate
    pub fn from_bitrate(bitrate_bps: u32) -> Self {
        if bitrate_bps < 1_000 {
            Self::Slow
        } else if bitrate_bps < 100_000 {
            Self::Medium
        } else {
            Self::Fast
        }
    }

    /// Get maximum window size for this speed
    pub fn max_window(&self) -> usize {
        match self {
            Self::Slow => WINDOW_MAX_SLOW,
            Self::Medium => WINDOW_MAX_MEDIUM,
            Self::Fast => WINDOW_MAX_FAST,
        }
    }

    /// Get minimum window limit for this speed
    pub fn min_window_limit(&self) -> usize {
        match self {
            Self::Slow => WINDOW_MIN_LIMIT_SLOW,
            Self::Medium => WINDOW_MIN_LIMIT_MEDIUM,
            Self::Fast => WINDOW_MIN_LIMIT_FAST,
        }
    }
}

/// Calculate packet timeout based on retry count and RTT
///
/// Formula: timeout = (1.5 ^ (tries - 1)) × max(RTT × 2.5, 25ms) × (tx_ring_size + 1.5)
///
/// Returns timeout in milliseconds
pub fn calculate_timeout(tries: u8, rtt_ms: u32, tx_ring_size: usize) -> u32 {
    // Base timeout: max(RTT * 2.5, 25ms)
    let base_rtt = rtt_ms.max(MIN_RTT_MS);
    let base_timeout = base_rtt * 25 / 10; // RTT * 2.5

    // Exponential backoff: 1.5 ^ (tries - 1)
    // Use fixed-point arithmetic (multiply by 1000 for precision)
    let backoff_factor = {
        let mut factor = 1000u32; // 1.0 in fixed point (x1000)
        for _ in 1..tries {
            factor = factor * 15 / 10; // multiply by 1.5
        }
        factor
    };

    // Ring size factor: (tx_ring_size + 1.5)
    let ring_factor = (tx_ring_size * 10 + 15) as u32; // x10 for fixed point

    // Final calculation: base_timeout * backoff * ring_factor
    // Divide by 10000 to account for fixed-point (1000 * 10)
    (base_timeout * backoff_factor * ring_factor) / 10000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_increment() {
        let mut seq = SequenceNumber::new(0);
        seq.increment();
        assert_eq!(seq.as_u16(), 1);

        let mut seq = SequenceNumber::new(SEQ_MAX);
        seq.increment();
        assert_eq!(seq.as_u16(), 0); // Wraparound
    }

    #[test]
    fn test_sequence_distance() {
        let seq1 = SequenceNumber::new(10);
        let seq2 = SequenceNumber::new(20);
        assert_eq!(seq1.distance_to(seq2), 10);

        // Test wraparound
        let seq1 = SequenceNumber::new(SEQ_MAX - 5);
        let seq2 = SequenceNumber::new(5);
        assert_eq!(seq1.distance_to(seq2), 11);
    }

    #[test]
    fn test_sequence_ordering() {
        let seq1 = SequenceNumber::new(10);
        let seq2 = SequenceNumber::new(20);
        assert!(seq1.is_before(seq2));
        assert!(!seq2.is_before(seq1));

        // Test wraparound
        let seq1 = SequenceNumber::new(SEQ_MAX - 5);
        let seq2 = SequenceNumber::new(5);
        assert!(seq1.is_before(seq2));
    }

    #[test]
    fn test_envelope_pack_unpack() {
        let msg_type = MessageType::new(42);
        let sequence = SequenceNumber::new(123);
        let payload = b"test payload";

        let envelope: Envelope<MAX_ENVELOPE_SIZE> = Envelope::new(msg_type, sequence, payload).expect("create envelope");
        let packed = envelope.pack();

        let unpacked: Envelope<MAX_ENVELOPE_SIZE> = Envelope::unpack(packed.as_slice()).expect("unpack envelope");

        assert_eq!(unpacked.msg_type, msg_type);
        assert_eq!(unpacked.sequence, sequence);
        assert_eq!(unpacked.payload_slice(), payload);
    }

    #[test]
    fn test_envelope_size() {
        let envelope: Envelope<MAX_ENVELOPE_SIZE> = Envelope::new(
            MessageType::new(1),
            SequenceNumber::new(0),
            b"hello"
        ).expect("create envelope");

        assert_eq!(envelope.size(), ENVELOPE_HEADER_SIZE + 5);
    }

    #[test]
    fn test_link_speed_classification() {
        assert_eq!(LinkSpeed::from_bitrate(500), LinkSpeed::Slow);
        assert_eq!(LinkSpeed::from_bitrate(50_000), LinkSpeed::Medium);
        assert_eq!(LinkSpeed::from_bitrate(200_000), LinkSpeed::Fast);
    }

    #[test]
    fn test_timeout_calculation() {
        // Base case: first try, 100ms RTT, empty tx ring
        let timeout = calculate_timeout(1, 100, 0);
        // Expected: 100 * 2.5 * 1.0 * 1.5 = 375ms
        assert!((370..=380).contains(&timeout), "timeout = {}", timeout);

        // Retry case: timeout should increase with tries
        let timeout1 = calculate_timeout(1, 100, 0);
        let timeout2 = calculate_timeout(2, 100, 0);
        assert!(timeout2 > timeout1);

        // With tx ring
        let timeout_with_ring = calculate_timeout(1, 100, 5);
        let timeout_no_ring = calculate_timeout(1, 100, 0);
        assert!(timeout_with_ring > timeout_no_ring);

        // Minimum RTT enforced
        let timeout = calculate_timeout(1, 10, 0);
        // Expected: 25 * 2.5 * 1.0 * 1.5 = 93.75ms
        assert!((90..=100).contains(&timeout), "timeout = {}", timeout);
    }
}
