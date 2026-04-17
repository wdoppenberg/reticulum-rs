//! Chat node — background tasks and user-facing handle.
//!
//! # Overview
//!
//! ```text
//!                    ┌──────────────┐
//!  announce_task ───▶│              │
//!  discovery_task ──▶│  ChatState   │◀── channel_rx_task (per link)
//!  out_link_task ───▶│  (Arc<Mutex>)│
//!  in_link_task ────▶│              │
//!                    └──────────────┘
//!                           │
//!                    broadcast::Sender<ChatEvent>
//!                           │
//!                    ┌──────┴──────┐
//!                    │  ChatHandle │  (cloneable, user-facing)
//!                    └─────────────┘
//! ```
//!
//! **Destination naming**: `reticulum_chat.text` — chosen to be recognisable
//! without conflicting with the Python LXMF stack.
//!
//! **Wire format**: see [`crate::message`].

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use reticulum_core::channel::types::{LinkSpeed, MessageType};
use reticulum_core::destination::DestinationDesc;
use reticulum_core::destination::DestinationName;
use reticulum_core::hash::AddressHash;
use reticulum_core::identity::PrivateIdentity;
use reticulum_tokio::channel::Channel;
use reticulum_tokio::link::{LinkEvent, LinkId};
use reticulum_tokio::transport::AnnounceEvent;
use reticulum_tokio::{ChannelReceiver, Transport};

use std::collections::hash_map::Entry;

use crate::cmd::{ChatCmd, TextMessage};
use crate::error::ChatError;
use crate::message::ChatMessage;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Reticulum destination app-name for the chat service.
pub const APP_NAME: &str = "reticulum_chat";
/// Aspects component of the destination name.
pub const APP_ASPECTS: &str = "text";

/// How often we re-broadcast our announce.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(10);

/// Short delay before the second announce, to reach peers whose TCP connection
/// comes up a few seconds after our initial announce was broadcast into the void.
const ANNOUNCE_EARLY_RETRY: Duration = Duration::from_secs(3);

/// Capacity of the ChatEvent broadcast channel.
const EVENT_CHANNEL_CAP: usize = 64;

// ── ChatEvent ─────────────────────────────────────────────────────────────────

/// Events emitted by the chat node.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// A new peer was discovered via a network announce.
    PeerDiscovered {
        /// The peer's Reticulum destination descriptor (contains address hash
        /// and public identity — pass to [`ChatHandle::connect`] to start a
        /// conversation).
        ///
        /// Boxed to keep the enum variant size uniform with the other variants
        /// (DestinationDesc is ~300 bytes; the next-largest variant is ~48).
        desc: Box<DestinationDesc>,
        /// Optional UTF-8 display name included by the peer in its announce.
        display_name: Option<String>,
    },

    /// A text message was received from a peer.
    MessageReceived { message: ChatMessage },

    /// A channel to a peer became ready for messaging.
    PeerConnected { peer_address: AddressHash },

    /// The channel to a peer was closed.
    PeerDisconnected { peer_address: AddressHash },
}

// ── Internal state ────────────────────────────────────────────────────────────

struct ChatState {
    /// Channels for which the remote peer's address is known.
    /// Key: peer's chat destination address hash.
    channels: HashMap<AddressHash, Arc<Channel>>,

    /// Channels on *incoming* links whose remote identity is not yet known
    /// (we have not yet received a message that includes their address).
    /// Key: link ID.
    pending_in: HashMap<LinkId, Arc<Channel>>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            channels: HashMap::new(),
            pending_in: HashMap::new(),
        }
    }
}

// ── ChatHandle ────────────────────────────────────────────────────────────────

/// Cloneable, user-facing handle for a running chat node.
///
/// Dropping the last clone shuts down the background tasks.
#[must_use = "dropping ChatHandle cancels the background chat tasks"]
#[derive(Clone)]
pub struct ChatHandle {
    /// Address hash of this node's chat destination.
    pub own_address: AddressHash,
    transport: Arc<Transport>,
    state: Arc<Mutex<ChatState>>,
    event_tx: broadcast::Sender<ChatEvent>,
    cancel: CancellationToken,
}

impl ChatHandle {
    /// Subscribe to [`ChatEvent`]s from this node.
    pub fn subscribe(&self) -> broadcast::Receiver<ChatEvent> {
        self.event_tx.subscribe()
    }

    /// Initiate an outgoing link to a peer (fire-and-forget).
    ///
    /// The link establishment is asynchronous; a [`ChatEvent::PeerConnected`]
    /// event will be broadcast once the channel is ready.
    ///
    /// Prefer [`peer`](Self::peer) when you need to await the connection or
    /// send messages with compile-time safety guarantees.
    pub async fn connect(&self, dest: DestinationDesc) {
        self.transport.link(dest).await;
    }

    /// Create a [`PeerHandle<Disconnected>`] for a discovered peer.
    ///
    /// Call [`PeerHandle::connect`] on the returned handle to initiate the
    /// link and await a [`PeerHandle<Connected>`] that can send messages
    /// without the possibility of a `ChatError::NoLink` error.
    pub fn peer(&self, dest: DestinationDesc) -> PeerHandle<Disconnected> {
        PeerHandle {
            peer_desc: dest,
            handle: self.clone(),
            _marker: PhantomData,
        }
    }

    /// Send a text message to a connected peer.
    ///
    /// Returns [`ChatError::NoLink`] if no active channel to `peer_address`
    /// exists. Call [`connect`](Self::connect) first and wait for
    /// [`ChatEvent::PeerConnected`].
    pub async fn send_message(
        &self,
        peer_address: AddressHash,
        content: &str,
    ) -> Result<(), ChatError> {
        let msg = TextMessage::new(self.own_address, content);
        let payload = msg.encode();

        let channel = self
            .state
            .lock()
            .await
            .channels
            .get(&peer_address)
            .cloned();

        match channel {
            Some(ch) => ch
                .send(MessageType::new(TextMessage::MSG_TYPE), &payload)
                .await
                .map(|_| ())
                .map_err(ChatError::Channel),
            None => Err(ChatError::NoLink),
        }
    }

    /// Returns the address hashes of all currently connected peers.
    pub async fn connected_peers(&self) -> Vec<AddressHash> {
        self.state
            .lock()
            .await
            .channels
            .keys()
            .cloned()
            .collect()
    }

    /// Shut down all background tasks for this node.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

// ── PeerHandle type-state ─────────────────────────────────────────────────────

/// State marker: peer is known but no channel has been established yet.
pub struct Disconnected;

/// State marker: a channel to the peer is open and ready for messaging.
pub struct Connected;

/// A typed handle to a specific remote peer.
///
/// The type parameter encodes the connection state so that `send` is only
/// callable once the channel is established:
///
/// ```text
/// ChatHandle::peer(desc) -> PeerHandle<Disconnected>
///     .connect(timeout)  -> Result<PeerHandle<Connected>, ChatError>
///         .send("hi!")   -> Result<(), ChannelError>
/// ```
///
/// `PeerHandle<Connected>::send` cannot return `ChatError::NoLink` — the
/// type guarantees that a channel existed at the moment `connect` resolved.
/// If the channel closes *after* `connect`, `send` returns
/// `ChannelError::LinkClosed` instead.
pub struct PeerHandle<S> {
    peer_desc: DestinationDesc,
    handle: ChatHandle,
    _marker: PhantomData<S>,
}

impl PeerHandle<Disconnected> {
    /// Subscribe to events, initiate the outgoing link, and wait until the
    /// channel is fully established.
    ///
    /// Subscribes to [`ChatEvent`]s *before* requesting the link so that a
    /// fast handshake cannot race past the subscription.
    pub async fn connect(self, timeout: Duration) -> Result<PeerHandle<Connected>, ChatError> {
        let peer_address = self.peer_desc.address_hash;

        // Subscribe before initiating the link — avoids the race where the
        // PeerConnected event fires before we start listening.
        let mut events = self.handle.event_tx.subscribe();
        self.handle.transport.link(self.peer_desc).await;

        tokio::time::timeout(timeout, async move {
            loop {
                match events.recv().await {
                    Ok(ChatEvent::PeerConnected { peer_address: addr }) if addr == peer_address => {
                        return Ok(PeerHandle {
                            peer_desc: self.peer_desc,
                            handle: self.handle,
                            _marker: PhantomData::<Connected>,
                        });
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Err(ChatError::Closed),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        })
        .await
        .map_err(|_| ChatError::Timeout)?
    }
}

impl PeerHandle<Connected> {
    /// Send a text message to the peer.
    ///
    /// Unlike [`ChatHandle::send_message`], this method **cannot** return
    /// `ChatError::NoLink`.  If the channel closed after `connect` resolved,
    /// it returns `reticulum_tokio::ChannelError::LinkClosed`.
    pub async fn send(&self, content: &str) -> Result<(), reticulum_tokio::ChannelError> {
        let own_address = self.handle.own_address;
        let peer_address = self.peer_desc.address_hash;
        let msg = TextMessage::new(own_address, content);
        let payload = msg.encode();

        let channel = self
            .handle
            .state
            .lock()
            .await
            .channels
            .get(&peer_address)
            .cloned();

        match channel {
            Some(ch) => ch
                .send(MessageType::new(TextMessage::MSG_TYPE), &payload)
                .await
                .map(|_| ()),
            // Channel closed between connect() and send() — surface as LinkClosed,
            // not NoLink (the connection *was* established).
            None => Err(reticulum_tokio::ChannelError::LinkClosed),
        }
    }

    /// The address hash of the remote peer's chat destination.
    pub fn peer_address(&self) -> AddressHash {
        self.peer_desc.address_hash
    }

    /// Subscribe to [`ChatEvent`]s from the underlying chat node.
    pub fn subscribe(&self) -> broadcast::Receiver<ChatEvent> {
        self.handle.subscribe()
    }
}

// ── start ─────────────────────────────────────────────────────────────────────

/// Start the chat node.
///
/// Registers a `reticulum_chat.text` destination with `transport`, spawns the
/// background announce / discovery / link / channel tasks, and returns a
/// [`ChatHandle`].
///
/// `display_name` is included as `app_data` in announce packets so that peers
/// can display a human-readable name instead of a raw address hash.
pub async fn start(
    transport: Arc<Transport>,
    identity: PrivateIdentity,
    display_name: Option<String>,
) -> Result<ChatHandle, ChatError> {
    let dest_name = DestinationName::new(APP_NAME, APP_ASPECTS);
    let app_data: Option<Vec<u8>> = display_name.map(|n| n.into_bytes());

    let dest = transport.add_destination(identity, dest_name).await;
    let own_address = dest.lock().await.desc.address_hash;

    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAP);
    let cancel = CancellationToken::new();
    let state = Arc::new(Mutex::new(ChatState::new()));

    let handle = ChatHandle {
        own_address,
        transport: transport.clone(),
        state: state.clone(),
        event_tx: event_tx.clone(),
        cancel: cancel.clone(),
    };

    // ── Task 1: Periodic announce ─────────────────────────────────────────────
    // Fire at t=0, t=EARLY_RETRY (covers peers whose TCP link comes up after
    // the initial announce), then every ANNOUNCE_INTERVAL thereafter.
    tokio::spawn({
        let transport = transport.clone();
        let dest = dest.clone();
        let app_data = app_data.clone();
        let cancel = cancel.clone();
        async move {
            let mut early_done = false;
            loop {
                transport
                    .send_announce(&dest, app_data.as_deref())
                    .await;
                let delay = if !early_done {
                    early_done = true;
                    ANNOUNCE_EARLY_RETRY
                } else {
                    ANNOUNCE_INTERVAL
                };
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            log::debug!("chat: announce task exited");
        }
    });

    // ── Task 2: Peer discovery ────────────────────────────────────────────────
    tokio::spawn({
        let mut announces = transport.recv_announces().await;
        let event_tx = event_tx.clone();
        let cancel = cancel.clone();
        async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = announces.recv() => match result {
                        Ok(ev) => emit_peer_discovered(ev, &event_tx).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("chat: discovery lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            log::debug!("chat: discovery task exited");
        }
    });

    // ── Task 3: Outgoing link activations ────────────────────────────────────
    tokio::spawn({
        let transport = transport.clone();
        let state = state.clone();
        let event_tx = event_tx.clone();
        let cancel = cancel.clone();
        let mut out_events = transport.out_link_events();
        async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = out_events.recv() => match result {
                        Ok(ev) => {
                            let peer_address = ev.address_hash;
                            match ev.event {
                                LinkEvent::Activated => {
                                    if let Some(link) = transport.find_active_out_link(&ev.id).await {
                                        let (ch, rx) = Channel::new(
                                            link,
                                            transport.clone(),
                                            transport.out_link_events(),
                                            transport.out_link_data(),
                                            LinkSpeed::Medium,
                                        ).await;
                                        let ch = Arc::new(ch);
                                        tokio::spawn(channel_rx_task(
                                            ev.id,
                                            rx,
                                            Some(peer_address),
                                            state.clone(),
                                            event_tx.clone(),
                                            None,
                                        ));
                                        state.lock().await.channels.insert(peer_address, ch);
                                        let _ = event_tx.send(ChatEvent::PeerConnected { peer_address });
                                        log::debug!("chat: outgoing channel to {} ready", peer_address);
                                    }
                                }
                                LinkEvent::Closed => {
                                    state.lock().await.channels.remove(&peer_address);
                                    let _ = event_tx.send(ChatEvent::PeerDisconnected { peer_address });
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("chat: out-link event receiver lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            log::debug!("chat: out-link task exited");
        }
    });

    // ── Task 4: Incoming link activations ────────────────────────────────────
    tokio::spawn({
        let transport = transport.clone();
        let state = state.clone();
        let event_tx = event_tx.clone();
        let cancel = cancel.clone();
        let mut in_events = transport.in_link_events();
        async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = in_events.recv() => match result {
                        Ok(ev) => match ev.event {
                            LinkEvent::Activated => {
                                if let Some(link) = transport.find_active_in_link(&ev.id).await {
                                    let (ch, rx) = Channel::new(
                                        link,
                                        transport.clone(),
                                        transport.in_link_events(),
                                        transport.in_link_data(),
                                        LinkSpeed::Medium,
                                    ).await;
                                    let ch = Arc::new(ch);
                                    // Pass a clone of the Arc so channel_rx_task
                                    // keeps the channel alive even if pending_in
                                    // drops its reference during promotion.
                                    tokio::spawn(channel_rx_task(
                                        ev.id,
                                        rx,
                                        None, // peer unknown until first message
                                        state.clone(),
                                        event_tx.clone(),
                                        Some(ch.clone()),
                                    ));
                                    state.lock().await.pending_in.insert(ev.id, ch);
                                    log::debug!("chat: incoming channel on link {} pending", ev.id);
                                }
                            }
                            LinkEvent::Closed => {
                                let mut s = state.lock().await;
                                if let Some(ch) = s.pending_in.remove(&ev.id) {
                                    // If the channel was promoted to `channels`, remove it there too.
                                    s.channels.retain(|_, v| !Arc::ptr_eq(v, &ch));
                                }
                            }
                        },
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("chat: in-link event receiver lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            log::debug!("chat: in-link task exited");
        }
    });

    Ok(handle)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Emit a [`ChatEvent::PeerDiscovered`] for a received announce.
async fn emit_peer_discovered(ev: AnnounceEvent, event_tx: &broadcast::Sender<ChatEvent>) {
    // Use an async lock (not try_lock) so we never silently drop an announce
    // because the destination mutex happened to be held by another task.
    let desc = ev.destination.lock().await.desc;

    let display_name = {
        let raw = ev.app_data.as_slice();
        if raw.is_empty() {
            None
        } else {
            String::from_utf8(raw.to_vec()).ok()
        }
    };

    let _ = event_tx.send(ChatEvent::PeerDiscovered { desc: Box::new(desc), display_name });
}

/// Per-channel receive loop.
///
/// Decodes incoming [`ChatMessage`]s and:
/// - For incoming links where `known_peer` is `None`: promotes the pending
///   channel entry to a named entry on first message — **unless** the peer
///   already has an out-link channel, in which case we deliver messages but
///   do not claim ownership of the `channels` entry.
/// - Emits [`ChatEvent::MessageReceived`] for every decoded message.
/// - Emits [`ChatEvent::PeerDisconnected`] when the channel closes, only if
///   this task owns the `channels` entry for the peer.
///
/// `keep_alive` should be `Some(ch.clone())` when spawned for incoming links,
/// so that the `Channel` is not dropped if `channels` already holds an entry
/// for the peer (which would cancel the channel's background tasks and
/// prevent further message delivery).
async fn channel_rx_task(
    link_id: LinkId,
    mut rx: ChannelReceiver,
    known_peer: Option<AddressHash>,
    state: Arc<Mutex<ChatState>>,
    event_tx: broadcast::Sender<ChatEvent>,
    keep_alive: Option<Arc<Channel>>,
) {
    // Holds an Arc<Channel> to prevent premature cancellation.  For incoming
    // links this is the Arc passed by the caller; it may be replaced during
    // promotion if the existing `channels` entry already covers this peer.
    let mut _keep_alive = keep_alive;

    // `resolved_peer` is Some when THIS task owns `channels[peer]` and is
    // responsible for emitting PeerDisconnected on close.
    let mut resolved_peer = known_peer;

    while let Some(msg) = rx.recv().await {
        if msg.msg_type.as_u16() != TextMessage::MSG_TYPE {
            continue;
        }

        let Some(chat_msg) = ChatMessage::decode(&msg.payload) else {
            log::warn!("chat: failed to decode message on link {}", link_id);
            continue;
        };

        let sender = chat_msg.sender;

        // First message on an incoming link: attempt to promote the channel.
        if resolved_peer.is_none() {
            let mut s = state.lock().await;
            if let Some(ch) = s.pending_in.remove(&link_id) {
                match s.channels.entry(sender) {
                    Entry::Vacant(e) => {
                        // No existing channel for this peer — take ownership.
                        e.insert(ch);
                        resolved_peer = Some(sender);
                        let _ = event_tx.send(ChatEvent::PeerConnected { peer_address: sender });
                    }
                    Entry::Occupied(_) => {
                        // The peer already has a channel (they also opened an
                        // out-link to us).  Keep the channel Arc alive so its
                        // background tasks keep running, but do not claim
                        // ownership of the `channels` entry — that entry belongs
                        // to the out-link task which will handle disconnect.
                        _keep_alive = Some(ch);
                        log::debug!(
                            "chat: incoming link {} from {} shadowed by existing channel; \
                             messages still delivered",
                            link_id,
                            sender
                        );
                    }
                }
            } else {
                // pending_in was already cleared without a ch (shouldn't
                // happen normally, but guard anyway).
                let s_ref = s;
                if !s_ref.channels.contains_key(&sender) {
                    resolved_peer = Some(sender);
                }
            }
        }

        let _ = event_tx.send(ChatEvent::MessageReceived { message: chat_msg });
    }

    // Channel closed: only clean up if we own the channels entry.
    if let Some(peer) = resolved_peer {
        state.lock().await.channels.remove(&peer);
        let _ = event_tx.send(ChatEvent::PeerDisconnected { peer_address: peer });
    }

    log::debug!("chat: channel_rx_task for link {} exited", link_id);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use getrandom::SysRng;\n    use rand_core::UnwrapErr;
    use reticulum_core::hash::AddressHash;
    use reticulum_core::identity::PrivateIdentity;
    use reticulum_tokio::channel::InboundMessage;
    use reticulum_core::channel::types::MessageType;
    use crate::cmd::{ChatCmd, TextMessage};

    // ── channel_rx_task unit tests ────────────────────────────────────────────

    /// Build a minimal inbound message carrying a text payload.
    fn make_text_inbound(sender: AddressHash, content: &str) -> InboundMessage {
        let msg = TextMessage::new(sender, content);
        InboundMessage {
            msg_type: MessageType::new(TextMessage::MSG_TYPE),
            payload: msg.encode(),
        }
    }

    /// An inbound message with an unknown message type (should be ignored).
    fn make_unknown_inbound(payload: Vec<u8>) -> InboundMessage {
        InboundMessage {
            msg_type: MessageType::new(0xABCD),
            payload,
        }
    }

    /// Drives `channel_rx_task` with a pre-built sequence of messages and
    /// returns the events that were broadcast.
    ///
    /// `known_peer`: mirrors the same argument in `channel_rx_task`.  When
    /// `Some`, a `PeerDisconnected` event is emitted when the channel closes.
    async fn run_rx_task(
        messages: Vec<InboundMessage>,
        known_peer: Option<AddressHash>,
    ) -> Vec<ChatEvent> {
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, mut event_rx) = broadcast::channel(64);
        let state = Arc::new(Mutex::new(ChatState::new()));
        let link_id = AddressHash::new_empty();

        // Feed all messages then drop the sender (signals EOF to task).
        for m in messages {
            inbound_tx.send(m).await.unwrap();
        }
        drop(inbound_tx);

        let receiver = reticulum_tokio::ChannelReceiver::from_receiver(inbound_rx);

        channel_rx_task(link_id, receiver, known_peer, state, event_tx, None).await;

        // Drain events.
        let mut out = Vec::new();
        while let Ok(ev) = event_rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// For a known peer a text message produces `MessageReceived` followed by
    /// `PeerDisconnected` when the channel closes (EOF).
    #[tokio::test]
    async fn rx_task_emits_message_then_disconnect_for_known_peer() {
        let sender = AddressHash::new_from_rand(OsRng);
        let events = run_rx_task(
            vec![make_text_inbound(sender, "hello")],
            Some(sender),
        )
        .await;

        assert_eq!(events.len(), 2, "expected MessageReceived + PeerDisconnected");
        match &events[0] {
            ChatEvent::MessageReceived { message } => {
                assert_eq!(message.sender, sender);
                assert_eq!(message.content, "hello");
            }
            other => panic!("expected MessageReceived, got {:?}", other),
        }
        match &events[1] {
            ChatEvent::PeerDisconnected { peer_address } => {
                assert_eq!(*peer_address, sender);
            }
            other => panic!("expected PeerDisconnected, got {:?}", other),
        }
    }

    /// Unknown message types are silently skipped; only `PeerDisconnected` is
    /// emitted when the channel closes.
    #[tokio::test]
    async fn rx_task_ignores_unknown_message_type() {
        let sender = AddressHash::new_from_rand(OsRng);
        let events = run_rx_task(
            vec![make_unknown_inbound(b"garbage".to_vec())],
            Some(sender),
        )
        .await;

        // The unknown message is silently dropped; EOF then closes the channel.
        assert_eq!(events.len(), 1, "expected only PeerDisconnected");
        match &events[0] {
            ChatEvent::PeerDisconnected { .. } => {}
            other => panic!("expected PeerDisconnected, got {:?}", other),
        }
    }

    /// When `known_peer` is `Some`, closing the channel without any messages
    /// still emits `PeerDisconnected`.
    #[tokio::test]
    async fn rx_task_emits_peer_disconnected_on_close_for_known_peer() {
        let sender = AddressHash::new_from_rand(OsRng);
        // Zero messages — task gets EOF immediately and emits PeerDisconnected.
        let events = run_rx_task(vec![], Some(sender)).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::PeerDisconnected { peer_address } => {
                assert_eq!(*peer_address, sender);
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    /// When `known_peer` is `None` (incoming link) and `pending_in` has no
    /// entry for the link (already promoted or state cleared), a text message
    /// produces `MessageReceived` (no `PeerConnected` because nothing to
    /// promote) followed by `PeerDisconnected` when the channel closes, because
    /// the task now resolves the peer address from the first message.
    #[tokio::test]
    async fn rx_task_delivers_message_for_unknown_peer_without_pending_entry() {
        let sender = AddressHash::new_from_rand(OsRng);
        let events = run_rx_task(
            vec![make_text_inbound(sender, "hi")],
            None, // unknown peer, pending_in is empty
        )
        .await;

        // MessageReceived + PeerDisconnected (peer resolved from first message).
        assert_eq!(events.len(), 2, "expected MessageReceived + PeerDisconnected, got {:?}", events);
        match &events[0] {
            ChatEvent::MessageReceived { message } => {
                assert_eq!(message.sender, sender);
                assert_eq!(message.content, "hi");
            }
            other => panic!("expected MessageReceived, got {:?}", other),
        }
        match &events[1] {
            ChatEvent::PeerDisconnected { peer_address } => {
                assert_eq!(*peer_address, sender);
            }
            other => panic!("expected PeerDisconnected, got {:?}", other),
        }
    }

    // Note: the "shadowed channel" path (where `channels` already has an entry
    // for the sender when an incoming-link task first processes a message) is
    // exercised at the integration level via `--tmp` two-instance runs.  It
    // cannot be unit-tested here because constructing a real `Arc<Channel>`
    // requires an `ActiveLink`, whose constructor is `pub(crate)` in
    // `reticulum-tokio`.

    // ── PeerHandle constructor tests ──────────────────────────────────────────

    #[tokio::test]
    async fn chat_handle_peer_returns_disconnected_handle() {
        // Verify that ChatHandle::peer() builds a PeerHandle without panicking
        // and that the peer address is preserved.
        let identity = PrivateIdentity::new_from_rand(OsRng);
        let own_address = *identity.address_hash();
        let (event_tx, _) = broadcast::channel(8);

        let transport = Arc::new(
            reticulum_tokio::Transport::new(reticulum_tokio::TransportConfig::new(
                "test",
                own_address,
                false,
            )),
        );

        let handle = ChatHandle {
            own_address,
            transport,
            state: Arc::new(Mutex::new(ChatState::new())),
            event_tx,
            cancel: CancellationToken::new(),
        };

        let peer_identity = PrivateIdentity::new_from_rand(OsRng);
        let peer_hash = *peer_identity.address_hash();
        let peer_desc = reticulum_core::destination::DestinationDesc {
            name: reticulum_core::destination::DestinationName::new(APP_NAME, APP_ASPECTS),
            identity: peer_identity.as_identity().clone(),
            address_hash: peer_hash,
        };

        let peer_handle = handle.peer(peer_desc);
        assert_eq!(peer_handle.peer_desc.address_hash, peer_hash);
    }

    // ── ChatError display tests ───────────────────────────────────────────────

    #[test]
    fn chat_error_no_link_displays() {
        let e = ChatError::NoLink;
        assert!(e.to_string().contains("no active channel"));
    }

    #[test]
    fn chat_error_timeout_displays() {
        let e = ChatError::Timeout;
        assert!(e.to_string().contains("timed out"));
    }

    #[test]
    fn chat_error_closed_displays() {
        assert!(ChatError::Closed.to_string().contains("closed"));
    }
}
