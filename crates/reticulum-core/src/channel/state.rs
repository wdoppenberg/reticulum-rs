//! Channel state machine - TX/RX rings and message tracking
//!
//! This module implements the sliding window protocol with TX and RX rings.
//!
//! # Feature flags
//!
//! | Feature    | Backing storage            | Notes                              |
//! |------------|----------------------------|------------------------------------|
//! | `alloc`    | `VecDeque` / `BTreeMap`    | Heap-allocated, dynamically sized  |
//! | `heapless` | `heapless::Deque` / `FnvIndexMap` | Stack-allocated, const-generic |
//!
//! When both features are enabled `alloc` takes precedence.
//!
//! For `no_alloc` targets enable `heapless` and disable `alloc`:
//! ```toml
//! reticulum-core = { default-features = false, features = ["heapless"] }
//! ```
//!
//! ## Out-of-order buffer capacity (`OOO`)
//!
//! When using the `heapless` feature, `RxRing<N, OOO>` requires `OOO` to be
//! a power of two (enforced by `heapless::FnvIndexMap`).

#[cfg(any(feature = "alloc", feature = "heapless"))]
use super::types::*;
#[cfg(any(feature = "alloc", feature = "heapless"))]
use crate::error::RnsError;

#[cfg(feature = "alloc")]
use alloc::collections::BTreeMap;
#[cfg(feature = "alloc")]
use alloc::collections::VecDeque;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
#[cfg(all(not(feature = "alloc"), feature = "heapless"))]
use heapless::index_map::FnvIndexMap;

// ──────────────────────────────────────────────────────────────────────────────
// TxMessageEntry
// ──────────────────────────────────────────────────────────────────────────────

/// Message entry in TX ring with state tracking
#[cfg(any(feature = "alloc", feature = "heapless"))]
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

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> TxMessageEntry<N> {
    /// Observe the current state without being able to mutate it directly.
    pub fn state(&self) -> MessageState {
        self.state
    }

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

// ──────────────────────────────────────────────────────────────────────────────
// RxMessageEntry
// ──────────────────────────────────────────────────────────────────────────────

/// RX message entry for tracking received messages
#[cfg(any(feature = "alloc", feature = "heapless"))]
#[derive(Debug, Clone)]
pub struct RxMessageEntry<const N: usize = MAX_ENVELOPE_SIZE> {
    pub envelope: Envelope<N>,
    pub received_ms: u64,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize> RxMessageEntry<N> {
    pub fn new(envelope: Envelope<N>, received_ms: u64) -> Self {
        Self {
            envelope,
            received_ms,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// TxRing
// ──────────────────────────────────────────────────────────────────────────────

/// TX Ring — manages outgoing messages with a sliding window protocol.
///
/// `N` is the maximum envelope size in bytes.
/// `W` is the ring's backing capacity (must be ≥ the link-speed max window).
/// When using the `alloc` feature `W` is still the compile-time maximum; the
/// runtime window grows up to `min(W, link_speed.max_window())`.
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct TxRing<const N: usize = MAX_ENVELOPE_SIZE, const W: usize = WINDOW_MAX_FAST> {
    #[cfg(feature = "alloc")]
    ring: VecDeque<TxMessageEntry<N>>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    ring: heapless::Deque<TxMessageEntry<N>, W>,

    /// Next sequence number to assign
    next_seq: SequenceNumber,
    /// Current (adaptive) window size
    window_size: usize,
    /// Maximum window size (capped by `W` for heapless)
    window_max: usize,
    /// Minimum window size
    window_min: usize,
    /// Sequence number of the oldest unacknowledged message
    next_ack_seq: SequenceNumber,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize, const W: usize> TxRing<N, W> {
    pub fn new(link_speed: LinkSpeed) -> Self {
        let window_max = {
            let lsmax = link_speed.max_window();
            // For heapless, cap at W so we never exceed backing capacity.
            if lsmax > W {
                W
            } else {
                lsmax
            }
        };
        Self {
            #[cfg(feature = "alloc")]
            ring: VecDeque::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            ring: heapless::Deque::new(),

            next_seq: SequenceNumber::zero(),
            window_size: WINDOW_INITIAL,
            window_max,
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

        #[cfg(feature = "alloc")]
        self.ring.push_back(entry);

        #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
        self.ring
            .push_back(entry)
            .map_err(|_| RnsError::WindowFull)?;

        self.next_seq.increment();
        Ok(seq)
    }

    /// Get mutable reference to an entry by sequence number.
    pub fn get_mut(&mut self, seq: SequenceNumber) -> Option<&mut TxMessageEntry<N>> {
        self.ring.iter_mut().find(|e| e.envelope.sequence == seq)
    }

    /// Invoke `f` for every sequence number that is ready to (re-)send.
    ///
    /// This is the allocation-free alternative to [`messages_to_send`].
    ///
    /// [`messages_to_send`]: Self::messages_to_send
    pub fn for_each_pending<F>(&self, current_time_ms: u64, mut f: F)
    where
        F: FnMut(SequenceNumber),
    {
        for entry in self.ring.iter() {
            let ready = match entry.state {
                MessageState::New => true,
                MessageState::Sent => entry.is_timed_out(current_time_ms) && entry.can_retry(),
                _ => false,
            };
            if ready {
                f(entry.envelope.sequence);
            }
        }
    }

    /// Collect sequence numbers ready to (re-)send into a `Vec`.
    ///
    /// Requires the `alloc` feature.  For `no_alloc` use [`for_each_pending`].
    ///
    /// [`for_each_pending`]: Self::for_each_pending
    #[cfg(feature = "alloc")]
    pub fn messages_to_send(&self, current_time_ms: u64) -> Vec<SequenceNumber> {
        let mut out = Vec::new();
        self.for_each_pending(current_time_ms, |seq| out.push(seq));
        out
    }

    /// Acknowledge a message by sequence number.
    ///
    /// Returns `true` if the sequence was found and delivered.
    pub fn acknowledge(&mut self, seq: SequenceNumber) -> bool {
        if let Some(entry) = self.ring.iter_mut().find(|e| e.envelope.sequence == seq) {
            entry.mark_delivered();

            // Slide the window: pop all consecutive delivered messages from the front.
            while let Some(front) = self.ring.front() {
                if front.state == MessageState::Delivered {
                    self.ring.pop_front();
                    self.next_ack_seq.increment();
                } else {
                    break;
                }
            }

            self.increase_window();
            return true;
        }
        false
    }

    /// Mark messages as permanently failed after retry exhaustion.
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
            self.decrease_window();
        }
    }

    fn increase_window(&mut self) {
        if self.window_size < self.window_max {
            self.window_size += 1;
        }
    }

    fn decrease_window(&mut self) {
        if self.window_size > self.window_min {
            self.window_size = (self.window_size - 1).max(self.window_min);
        }
    }

    /// Current adaptive window size.
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Number of outstanding (unacknowledged) messages.
    pub fn outstanding(&self) -> usize {
        self.ring
            .iter()
            .filter(|e| e.state == MessageState::Sent || e.state == MessageState::New)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// RxRing
// ──────────────────────────────────────────────────────────────────────────────

/// RX Ring — manages incoming messages with reorder buffering.
///
/// `N` is the maximum envelope size in bytes.
/// `OOO` is the out-of-order buffer capacity.
///
/// **`heapless` feature requirement:** `OOO` must be a power of two (enforced
/// by `heapless::FnvIndexMap` at compile time).
#[cfg(any(feature = "alloc", feature = "heapless"))]
pub struct RxRing<const N: usize = MAX_ENVELOPE_SIZE, const OOO: usize = 64> {
    #[cfg(feature = "alloc")]
    out_of_order: BTreeMap<u16, RxMessageEntry<N>>,
    #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
    out_of_order: FnvIndexMap<u16, RxMessageEntry<N>, OOO>,

    /// Next expected sequence number
    next_expected: SequenceNumber,

    /// Maximum out-of-order buffer size (alloc only; heapless uses `OOO`).
    #[cfg(feature = "alloc")]
    max_out_of_order: usize,
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize, const OOO: usize> RxRing<N, OOO> {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "alloc")]
            out_of_order: BTreeMap::new(),
            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            out_of_order: FnvIndexMap::new(),

            next_expected: SequenceNumber::zero(),

            #[cfg(feature = "alloc")]
            max_out_of_order: OOO,
        }
    }

    fn ooo_capacity(&self) -> usize {
        #[cfg(feature = "alloc")]
        {
            self.max_out_of_order
        }
        #[cfg(not(feature = "alloc"))]
        {
            OOO
        }
    }

    /// Process an incoming envelope, calling `on_ready` for each message that
    /// can now be delivered in order.
    ///
    /// This is the allocation-free core.  For alloc convenience use [`receive`].
    ///
    /// [`receive`]: Self::receive
    pub fn receive_with<F>(
        &mut self,
        envelope: Envelope<N>,
        current_time_ms: u64,
        mut on_ready: F,
    ) -> Result<(), RnsError>
    where
        F: FnMut(Envelope<N>),
    {
        let seq = envelope.sequence;

        // Duplicate / already-delivered check.
        if seq.is_before(self.next_expected) {
            return Ok(());
        }

        if seq == self.next_expected {
            on_ready(envelope);
            self.next_expected.increment();

            // Drain any buffered consecutive messages.
            loop {
                let key = self.next_expected.as_u16();
                let next_entry = self.out_of_order.remove(&key);
                match next_entry {
                    Some(entry) => {
                        on_ready(entry.envelope);
                        self.next_expected.increment();
                    }
                    None => break,
                }
            }
        } else {
            // Out-of-order: buffer for later delivery.
            if self.out_of_order.len() >= self.ooo_capacity() {
                return Err(RnsError::WindowFull);
            }
            let entry = RxMessageEntry::new(envelope, current_time_ms);

            #[cfg(feature = "alloc")]
            {
                self.out_of_order.insert(seq.as_u16(), entry);
            }

            #[cfg(all(not(feature = "alloc"), feature = "heapless"))]
            self.out_of_order
                .insert(seq.as_u16(), entry)
                .map_err(|_| RnsError::WindowFull)?;
        }

        Ok(())
    }

    /// Process an incoming envelope, returning all now-deliverable messages.
    ///
    /// Requires the `alloc` feature.  For `no_alloc` use [`receive_with`].
    ///
    /// [`receive_with`]: Self::receive_with
    #[cfg(feature = "alloc")]
    pub fn receive(
        &mut self,
        envelope: Envelope<N>,
        current_time_ms: u64,
    ) -> Result<Vec<Envelope<N>>, RnsError> {
        let mut ready = Vec::new();
        self.receive_with(envelope, current_time_ms, |env| ready.push(env))?;
        Ok(ready)
    }

    /// Get next expected sequence number.
    pub fn next_expected(&self) -> SequenceNumber {
        self.next_expected
    }

    /// Number of buffered out-of-order messages.
    pub fn buffered_count(&self) -> usize {
        self.out_of_order.len()
    }
}

#[cfg(any(feature = "alloc", feature = "heapless"))]
impl<const N: usize, const OOO: usize> Default for RxRing<N, OOO> {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

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

        for i in 0..WINDOW_INITIAL {
            tx_ring
                .push(MessageType::new(1), &[i as u8])
                .expect("push within window");
        }

        let result = tx_ring.push(MessageType::new(1), b"overflow");
        assert!(result.is_err());
    }

    #[test]
    fn test_tx_ring_acknowledge() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let seq0 = tx_ring.push(MessageType::new(1), b"msg0").expect("push");
        let seq1 = tx_ring.push(MessageType::new(1), b"msg1").expect("push");

        assert_eq!(tx_ring.len(), 2);

        tx_ring.get_mut(seq0).unwrap().mark_sent(0, 100, 2);
        tx_ring.get_mut(seq1).unwrap().mark_sent(0, 100, 2);

        assert!(tx_ring.acknowledge(seq0));
        assert_eq!(tx_ring.len(), 1);

        assert!(tx_ring.acknowledge(seq1));
        assert_eq!(tx_ring.len(), 0);
    }

    #[test]
    fn test_tx_ring_window_adaptation() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let initial_window = tx_ring.window_size();

        let seq = tx_ring.push(MessageType::new(1), b"test").expect("push");
        tx_ring.get_mut(seq).unwrap().mark_sent(0, 100, 1);
        tx_ring.acknowledge(seq);

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

        let env2: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(1), b"msg2").expect("envelope");
        let ready = rx_ring.receive(env2, 0).expect("receive");
        assert_eq!(ready.len(), 0);

        assert_eq!(rx_ring.buffered_count(), 1);

        let env1: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"msg1").expect("envelope");
        let ready = rx_ring.receive(env1, 0).expect("receive");
        assert_eq!(ready.len(), 2);

        assert_eq!(rx_ring.buffered_count(), 0);
    }

    #[test]
    fn test_rx_ring_duplicate() {
        let mut rx_ring: RxRing = RxRing::new();

        let env: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"msg").expect("envelope");

        let ready1 = rx_ring.receive(env.clone(), 0).expect("receive");
        assert_eq!(ready1.len(), 1);

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

        assert!(!entry.is_timed_out(1000 + 100));
        assert!(entry.is_timed_out(1000 + entry.timeout_ms as u64 + 1));
    }

    #[test]
    fn test_for_each_pending() {
        let mut tx_ring: TxRing = TxRing::new(LinkSpeed::Medium);

        let seq0 = tx_ring.push(MessageType::new(1), b"a").expect("push");
        let seq1 = tx_ring.push(MessageType::new(1), b"b").expect("push");

        let mut pending = Vec::new();
        tx_ring.for_each_pending(0, |s| pending.push(s));
        assert_eq!(pending, [seq0, seq1]);
    }

    #[test]
    fn test_receive_with() {
        let mut rx_ring: RxRing = RxRing::new();

        let env: Envelope =
            Envelope::new(MessageType::new(1), SequenceNumber::new(0), b"x").expect("envelope");

        let mut count = 0usize;
        rx_ring
            .receive_with(env, 0, |_| count += 1)
            .expect("receive_with");
        assert_eq!(count, 1);
    }
}
