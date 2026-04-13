//! Channel state machine - TX/RX rings and message tracking
//!
//! This module implements the sliding window protocol with TX and RX rings.

use super::types::*;
use crate::error::RnsError;

#[cfg(feature = "alloc")]
use alloc::collections::VecDeque;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
extern crate std;
#[cfg(feature = "alloc")]
use std::collections::HashMap;

/// Message entry in TX ring with state tracking
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub struct TxMessageEntry<const N: usize = MAX_ENVELOPE_SIZE> {
    /// The envelope to send
    pub envelope: Envelope<N>,
    /// Current state — `pub(crate)` to prevent external code from bypassing
    /// the state-transition methods (`mark_sent`, etc.).
    pub(crate) state: MessageState,
    /// Number of transmission attempts
    pub tries: u8,
    /// Timestamp of last transmission (milliseconds since epoch)
    pub last_sent_ms: Option<u64>,
    /// Calculated timeout for this message (milliseconds)
    pub timeout_ms: u32,
}

impl<const N: usize> TxMessageEntry<N> {
    /// Observe the current state without being able to mutate it directly.
    pub fn state(&self) -> MessageState {
        self.state
    }
}

impl<const N: usize> TxMessageEntry<N> {
    pub fn new(envelope: Envelope<N>) -> Self {
        Self {
            envelope,
            state: MessageState::New,
            tries: 0,
            last_sent_ms: None,
            timeout_ms: 0,
        }
    }

    /// Check if this message has timed out
    pub fn is_timed_out(&self, current_time_ms: u64) -> bool {
        if let Some(last_sent) = self.last_sent_ms {
            if self.state == MessageState::Sent {
                return current_time_ms - last_sent > self.timeout_ms as u64;
            }
        }
        false
    }

    /// Mark as sent and calculate timeout
    pub fn mark_sent(&mut self, current_time_ms: u64, rtt_ms: u32, tx_ring_size: usize) {
        self.state = MessageState::Sent;
        self.tries += 1;
        self.last_sent_ms = Some(current_time_ms);
        self.timeout_ms = calculate_timeout(self.tries, rtt_ms, tx_ring_size);
    }

    /// Mark as delivered.  Private: only `TxRing::acknowledge` may call this,
    /// ensuring `Delivered` is only reached from `Sent`.
    fn mark_delivered(&mut self) {
        debug_assert_eq!(
            self.state,
            MessageState::Sent,
            "mark_delivered called on a message that was not Sent"
        );
        self.state = MessageState::Delivered;
    }

    /// Mark as permanently failed.  Private: only `TxRing::check_failures` may
    /// call this, ensuring `Failed` is only reached after retry exhaustion.
    fn mark_failed(&mut self) {
        debug_assert_eq!(
            self.state,
            MessageState::Sent,
            "mark_failed called on a message that was not Sent"
        );
        self.state = MessageState::Failed;
    }

    /// Check if retry limit exceeded
    pub fn can_retry(&self) -> bool {
        self.tries < MAX_RETRIES
    }
}

/// RX message entry for tracking received messages
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub struct RxMessageEntry<const N: usize = MAX_ENVELOPE_SIZE> {
    pub envelope: Envelope<N>,
    pub received_ms: u64,
}

impl<const N: usize> RxMessageEntry<N> {
    pub fn new(envelope: Envelope<N>, received_ms: u64) -> Self {
        Self {
            envelope,
            received_ms,
        }
    }
}

/// TX Ring - Manages outgoing messages with sliding window
#[cfg(feature = "alloc")]
pub struct TxRing<const N: usize = MAX_ENVELOPE_SIZE> {
    /// Ring buffer of messages awaiting acknowledgment
    ring: VecDeque<TxMessageEntry<N>>,
    /// Next sequence number to assign
    next_seq: SequenceNumber,
    /// Current window size
    window_size: usize,
    /// Maximum window size
    window_max: usize,
    /// Minimum window size
    window_min: usize,
    /// Sequence number of next message to be acknowledged
    next_ack_seq: SequenceNumber,
}

#[cfg(feature = "alloc")]
impl<const N: usize> TxRing<N> {
    pub fn new(link_speed: LinkSpeed) -> Self {
        Self {
            ring: VecDeque::new(),
            next_seq: SequenceNumber::zero(),
            window_size: WINDOW_INITIAL,
            window_max: link_speed.max_window(),
            window_min: link_speed.min_window_limit(),
            next_ack_seq: SequenceNumber::zero(),
        }
    }

    /// Add a new message to the TX ring.
    ///
    /// Returns `Err(RnsError::WindowFull)` when all window slots are in use.
    pub fn push(
        &mut self,
        msg_type: MessageType,
        payload: &[u8],
    ) -> Result<SequenceNumber, RnsError> {
        if self.ring.len() >= self.window_size {
            return Err(RnsError::WindowFull);
        }

        let seq = self.next_seq;
        let envelope = Envelope::new(msg_type, seq, payload)?;
        let entry = TxMessageEntry::new(envelope);

        self.ring.push_back(entry);
        self.next_seq.increment();

        Ok(seq)
    }

    /// Get messages ready to send (not sent or timed out)
    ///
    /// Returns sequence numbers of messages to send
    pub fn messages_to_send(&self, current_time_ms: u64) -> Vec<SequenceNumber> {
        self.ring
            .iter()
            .filter(|entry| match entry.state {
                MessageState::New => true,
                MessageState::Sent => entry.is_timed_out(current_time_ms) && entry.can_retry(),
                _ => false,
            })
            .map(|entry| entry.envelope.sequence)
            .collect()
    }

    /// Get mutable reference to an entry by sequence number
    pub fn get_mut(&mut self, seq: SequenceNumber) -> Option<&mut TxMessageEntry<N>> {
        self.ring.iter_mut().find(|e| e.envelope.sequence == seq)
    }

    /// Acknowledge a message by sequence number
    pub fn acknowledge(&mut self, seq: SequenceNumber) -> bool {
        // Find and mark the message as delivered via the controlled transition.
        if let Some(entry) = self.ring.iter_mut().find(|e| e.envelope.sequence == seq) {
            entry.mark_delivered();

            // Remove all delivered messages from the front
            while let Some(front) = self.ring.front() {
                if front.state == MessageState::Delivered {
                    self.ring.pop_front();
                    self.next_ack_seq.increment();
                } else {
                    break;
                }
            }

            // Increase window on successful delivery
            self.increase_window();
            return true;
        }
        false
    }

    /// Mark messages as failed if they exceeded retry limit
    pub fn check_failures(&mut self, current_time_ms: u64) {
        let mut failed = false;
        for entry in self.ring.iter_mut() {
            if entry.state == MessageState::Sent
                && entry.is_timed_out(current_time_ms)
                && !entry.can_retry()
            {
                entry.mark_failed();
                failed = true;
            }
        }
        if failed {
            // Decrease window on failure
            self.decrease_window();
        }
    }

    /// Increase window size (on successful delivery)
    fn increase_window(&mut self) {
        if self.window_size < self.window_max {
            self.window_size += 1;
        }
    }

    /// Decrease window size (on failure or timeout)
    fn decrease_window(&mut self) {
        if self.window_size > self.window_min {
            self.window_size = (self.window_size - 1).max(self.window_min);
        }
    }

    /// Get current window size
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Get number of outstanding (unacknowledged) messages
    pub fn outstanding(&self) -> usize {
        self.ring
            .iter()
            .filter(|e| e.state == MessageState::Sent || e.state == MessageState::New)
            .count()
    }

    /// Check if ring is empty
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Get ring size
    pub fn len(&self) -> usize {
        self.ring.len()
    }
}

/// RX Ring - Manages incoming messages with ordering
#[cfg(feature = "alloc")]
pub struct RxRing<const N: usize = MAX_ENVELOPE_SIZE> {
    /// Messages received out of order
    out_of_order: HashMap<u16, RxMessageEntry<N>>,
    /// Next expected sequence number
    next_expected: SequenceNumber,
    /// Maximum out-of-order buffer size
    max_out_of_order: usize,
}

#[cfg(feature = "alloc")]
impl<const N: usize> Default for RxRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RxRing<N> {
    pub fn new() -> Self {
        Self {
            out_of_order: HashMap::new(),
            next_expected: SequenceNumber::zero(),
            max_out_of_order: 64, // Reasonable buffer size
        }
    }

    /// Process incoming envelope
    ///
    /// Returns a list of envelopes ready for delivery (in order)
    pub fn receive(
        &mut self,
        envelope: Envelope<N>,
        current_time_ms: u64,
    ) -> Result<Vec<Envelope<N>>, RnsError> {
        let seq = envelope.sequence;

        // Check if this is a duplicate
        if seq.is_before(self.next_expected) {
            // Old message, already delivered
            return Ok(Vec::new());
        }

        let mut ready = Vec::new();

        if seq == self.next_expected {
            // This is the next expected message
            ready.push(envelope);
            self.next_expected.increment();

            // Check if we have subsequent messages in the out-of-order buffer
            while let Some(entry) = self.out_of_order.remove(&self.next_expected.as_u16()) {
                ready.push(entry.envelope);
                self.next_expected.increment();
            }
        } else {
            // Out of order message - buffer it
            if self.out_of_order.len() >= self.max_out_of_order {
                return Err(RnsError::WindowFull);
            }

            let entry = RxMessageEntry::new(envelope, current_time_ms);
            self.out_of_order.insert(seq.as_u16(), entry);
        }

        Ok(ready)
    }

    /// Get next expected sequence number
    pub fn next_expected(&self) -> SequenceNumber {
        self.next_expected
    }

    /// Get number of buffered out-of-order messages
    pub fn buffered_count(&self) -> usize {
        self.out_of_order.len()
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;

    #[test]
    fn test_tx_ring_push() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let seq1 = tx_ring
            .push(MessageType::new(1), b"message 1")
            .expect("push");
        assert_eq!(seq1.as_u16(), 0);

        let seq2 = tx_ring
            .push(MessageType::new(1), b"message 2")
            .expect("push");
        assert_eq!(seq2.as_u16(), 1);

        assert_eq!(tx_ring.len(), 2);
    }

    #[test]
    fn test_tx_ring_window_limit() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Slow);

        // Initial window size is 2, not WINDOW_MAX_SLOW
        // Fill up to initial window size
        for i in 0..WINDOW_INITIAL {
            tx_ring
                .push(MessageType::new(1), &[i as u8])
                .expect("push within window");
        }

        // Next push should fail (window full)
        let result = tx_ring.push(MessageType::new(1), b"overflow");
        assert!(result.is_err());
    }

    #[test]
    fn test_tx_ring_acknowledge() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let seq0 = tx_ring.push(MessageType::new(1), b"msg0").expect("push");
        let seq1 = tx_ring.push(MessageType::new(1), b"msg1").expect("push");

        assert_eq!(tx_ring.len(), 2);

        // Messages must be marked sent before they can be acknowledged (protocol flow)
        tx_ring.get_mut(seq0).unwrap().mark_sent(0, 100, 2);
        tx_ring.get_mut(seq1).unwrap().mark_sent(0, 100, 2);

        // Acknowledge first message
        assert!(tx_ring.acknowledge(seq0));
        assert_eq!(tx_ring.len(), 1);

        // Acknowledge second message
        assert!(tx_ring.acknowledge(seq1));
        assert_eq!(tx_ring.len(), 0);
    }

    #[test]
    fn test_tx_ring_window_adaptation() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let initial_window = tx_ring.window_size();

        // Push, mark sent, then acknowledge (protocol order)
        let seq = tx_ring.push(MessageType::new(1), b"test").expect("push");
        tx_ring.get_mut(seq).unwrap().mark_sent(0, 100, 1);
        tx_ring.acknowledge(seq);

        // Window should increase
        assert!(tx_ring.window_size() > initial_window);
    }

    #[test]
    fn test_rx_ring_in_order() {
        let mut rx_ring: RxRing = RxRing::new();

        let env1: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"msg1").expect("envelope");
        let env2: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(1), b"msg2").expect("envelope");

        let ready1 = rx_ring.receive(env1, 0).expect("receive");
        assert_eq!(ready1.len(), 1);

        let ready2 = rx_ring.receive(env2, 0).expect("receive");
        assert_eq!(ready2.len(), 1);
    }

    #[test]
    fn test_rx_ring_out_of_order() {
        let mut rx_ring: RxRing = RxRing::new();

        // Receive message 2 before message 1
        let env2: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(1), b"msg2").expect("envelope");
        let ready = rx_ring.receive(env2, 0).expect("receive");
        assert_eq!(ready.len(), 0); // Buffered, not ready

        assert_eq!(rx_ring.buffered_count(), 1);

        // Now receive message 1
        let env1: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"msg1").expect("envelope");
        let ready = rx_ring.receive(env1, 0).expect("receive");
        assert_eq!(ready.len(), 2); // Both messages now ready

        assert_eq!(rx_ring.buffered_count(), 0);
    }

    #[test]
    fn test_rx_ring_duplicate() {
        let mut rx_ring: RxRing = RxRing::new();

        let env: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"msg").expect("envelope");

        let ready1 = rx_ring.receive(env.clone(), 0).expect("receive");
        assert_eq!(ready1.len(), 1);

        // Duplicate should be ignored
        let ready2 = rx_ring.receive(env, 0).expect("receive");
        assert_eq!(ready2.len(), 0);
    }

    #[test]
    fn test_tx_message_timeout() {
        let envelope: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"test").expect("envelope");
        let mut entry = TxMessageEntry::new(envelope);

        entry.mark_sent(1000, 100, 0);
        assert_eq!(entry.state(), MessageState::Sent);
        assert_eq!(entry.tries, 1);

        // Not timed out yet
        assert!(!entry.is_timed_out(1000 + 100));

        // Timed out
        assert!(entry.is_timed_out(1000 + entry.timeout_ms as u64 + 1));
    }
}
