//! Application state.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_chat::{ChatEvent, ChatHandle, ChatMessage};
use reticulum_core::destination::DestinationDesc;
use reticulum_core::hash::AddressHash;
use reticulum_tokio::config::AutoInterfaceConfig;
use reticulum_tokio::{Config, ReticulumPaths};

/// Maximum messages kept per conversation.
const MAX_MESSAGES: usize = 500;

/// Number of navigable items in the settings screen.
/// Indices 0-9: ReticulumConfig booleans
/// Indices 10-11: ReticulumConfig ports
/// Index 12: loglevel
pub const SETTINGS_ITEM_COUNT: usize = 13;

/// A discovered (and potentially connected) peer.
#[derive(Clone)]
pub struct Peer {
    pub desc: DestinationDesc,
    pub display_name: Option<String>,
    pub connected: bool,
}

impl Peer {
    fn label(&self) -> String {
        match &self.display_name {
            Some(name) => format!("{} ({})", name, short_addr(self.desc.address_hash)),
            None => short_addr(self.desc.address_hash),
        }
    }
}

/// A single entry in a conversation view.
#[derive(Clone)]
pub struct MessageEntry {
    pub sender: AddressHash,
    pub timestamp: u64,
    pub content: String,
    pub outgoing: bool,
}

impl MessageEntry {
    fn from_chat_msg(msg: &ChatMessage, own_address: AddressHash) -> Self {
        Self {
            sender: msg.sender,
            timestamp: msg.timestamp,
            content: msg.content.clone(),
            outgoing: msg.sender == own_address,
        }
    }

    pub fn time_str(&self) -> String {
        let secs = self.timestamp;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }
}

/// Which panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Peers,
    Input,
    /// Configuration / settings screen.
    Settings,
}

/// Top-level application state.
pub struct App {
    pub handle: ChatHandle,

    /// Ordered list of known peers (append-only; marked connected/disconnected).
    pub peers: Vec<Peer>,

    /// Conversations: peer address → messages.
    pub conversations: HashMap<AddressHash, VecDeque<MessageEntry>>,

    /// Index of the currently selected peer in `peers`.
    pub selected_peer: Option<usize>,

    /// Current text in the input box.
    pub input: String,

    /// Where keyboard focus is.
    pub focus: Focus,

    /// Status bar text (latest status or error).
    pub status: String,

    /// Whether the TUI should exit.
    pub should_quit: bool,

    /// Live copy of the Reticulum configuration (may be edited in the settings
    /// screen and written back to disk with `settings_save`).
    pub config: Config,

    /// Path to the config file on disk.
    pub config_path: PathBuf,

    /// Currently highlighted row in the settings screen (0-based).
    pub settings_cursor: usize,

    /// Whether this is a temporary instance (shown in UI).
    pub is_tmp: bool,
}

impl App {
    pub fn new(handle: ChatHandle, config: Config, paths: ReticulumPaths, is_tmp: bool) -> Self {
        Self {
            handle,
            peers: Vec::new(),
            conversations: HashMap::new(),
            selected_peer: None,
            input: String::new(),
            focus: Focus::Peers,
            status: "Reticulum Chat  [s] settings  [c] connect  [q] quit".to_string(),
            should_quit: false,
            config_path: paths.config_path,
            config,
            settings_cursor: 0,
            is_tmp,
        }
    }

    // ── Peer list navigation ──────────────────────────────────────────────────

    pub fn select_next_peer(&mut self) {
        if self.peers.is_empty() {
            return;
        }
        self.selected_peer = Some(match self.selected_peer {
            None => 0,
            Some(i) => (i + 1).min(self.peers.len() - 1),
        });
    }

    pub fn select_prev_peer(&mut self) {
        if self.peers.is_empty() {
            return;
        }
        self.selected_peer = Some(match self.selected_peer {
            None => 0,
            Some(i) => i.saturating_sub(1),
        });
    }

    pub fn selected_peer_address(&self) -> Option<AddressHash> {
        self.selected_peer
            .and_then(|i| self.peers.get(i))
            .map(|p| p.desc.address_hash)
    }

    pub fn peer_label(&self, idx: usize) -> String {
        self.peers[idx].label()
    }

    // ── Input handling ────────────────────────────────────────────────────────

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
    }

    /// Send the current input to the selected peer.
    pub async fn send_current_input(&mut self) {
        let content = self.input.trim().to_string();
        if content.is_empty() {
            return;
        }

        let Some(peer_addr) = self.selected_peer_address() else {
            self.status = "No peer selected.".to_string();
            return;
        };

        match self.handle.send_message(peer_addr, &content).await {
            Ok(()) => {
                let msg = MessageEntry {
                    sender: self.handle.own_address,
                    timestamp: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    content,
                    outgoing: true,
                };
                self.push_message(peer_addr, msg);
                self.clear_input();
            }
            Err(e) => {
                self.status = format!("Send error: {}", e);
            }
        }
    }

    /// Connect to the currently selected peer.
    pub async fn connect_selected(&mut self) {
        let Some(idx) = self.selected_peer else {
            self.status = "No peer selected.".to_string();
            return;
        };
        let peer = self.peers[idx].clone();
        self.status = format!("Connecting to {}…", short_addr(peer.desc.address_hash));
        self.handle.connect(peer.desc).await;
    }

    // ── Settings navigation ───────────────────────────────────────────────────

    pub fn settings_nav_up(&mut self) {
        self.settings_cursor = self.settings_cursor.saturating_sub(1);
    }

    pub fn settings_nav_down(&mut self) {
        self.settings_cursor = (self.settings_cursor + 1).min(SETTINGS_ITEM_COUNT - 1);
    }

    /// Toggle the currently selected setting (booleans) or increment (numbers).
    pub fn settings_activate(&mut self) {
        let r = &mut self.config.reticulum;
        let l = &mut self.config.logging;
        match self.settings_cursor {
            0 => {
                r.enable_transport = !r.enable_transport;
            }
            1 => {
                r.share_instance = !r.share_instance;
            }
            2 => {
                r.link_mtu_discovery = !r.link_mtu_discovery;
            }
            3 => {
                r.use_implicit_proof = !r.use_implicit_proof;
            }
            4 => {
                r.allow_probes = !r.allow_probes;
            }
            5 => {
                r.enable_remote_management = !r.enable_remote_management;
            }
            6 => {
                r.enable_discovery = !r.enable_discovery;
            }
            7 => {
                r.discover_interfaces = !r.discover_interfaces;
            }
            8 => {
                r.autoconnect_discovered_interfaces = !r.autoconnect_discovered_interfaces;
            }
            9 => {
                r.panic_on_interface_error = !r.panic_on_interface_error;
            }
            10 => {
                r.shared_instance_port = r.shared_instance_port.wrapping_add(1);
            }
            11 => {
                r.instance_control_port = r.instance_control_port.wrapping_add(1);
            }
            12 if l.loglevel < 7 => {
                l.loglevel += 1;
            }
            _ => {}
        }
        self.status = "Modified (unsaved). Press [w] to write to disk.".to_string();
    }

    /// Decrement numeric settings (no-op for booleans).
    pub fn settings_decrement(&mut self) {
        let r = &mut self.config.reticulum;
        let l = &mut self.config.logging;
        match self.settings_cursor {
            10 => {
                r.shared_instance_port = r.shared_instance_port.saturating_sub(1);
            }
            11 => {
                r.instance_control_port = r.instance_control_port.saturating_sub(1);
            }
            12 if l.loglevel > 0 => {
                l.loglevel -= 1;
            }
            _ => {}
        }
        self.status = "Modified (unsaved). Press [w] to write to disk.".to_string();
    }

    /// Toggle the `auto` AutoInterface in the in-memory config.
    ///
    /// If present and enabled → disabled.
    /// If present and disabled → enabled.
    /// If absent → added and enabled.
    pub fn settings_toggle_auto_iface(&mut self) {
        use reticulum_tokio::config::InterfaceConfig;
        let entry = self
            .config
            .interfaces
            .entry("auto".to_string())
            .or_insert_with(|| {
                InterfaceConfig::Auto(AutoInterfaceConfig {
                    enabled: false,
                    group: None,
                    discovery_port: None,
                    data_port: None,
                })
            });

        match entry {
            InterfaceConfig::Auto(a) => {
                a.enabled = !a.enabled;
                self.status = format!(
                    "AutoInterface {}. Press [w] to save.",
                    if a.enabled { "enabled" } else { "disabled" }
                );
            }
            _ => {
                self.status =
                    "An interface named 'auto' exists but is not AutoInterface.".to_string();
            }
        }
    }

    /// Write the in-memory config back to the config file.
    pub fn settings_save(&mut self) {
        match self.config.to_file(&self.config_path) {
            Ok(()) => {
                self.status = format!(
                    "Config saved to {}. Restart to apply.",
                    self.config_path.display()
                );
            }
            Err(e) => {
                self.status = format!("Save failed: {}", e);
            }
        }
    }

    // ── Event processing ──────────────────────────────────────────────────────

    pub fn process_event(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::PeerDiscovered { desc, display_name } => {
                let addr = desc.address_hash;
                if self.peers.iter().any(|p| p.desc.address_hash == addr) {
                    return; // already known
                }
                let label = match &display_name {
                    Some(n) => format!("{} ({})", n, short_addr(addr)),
                    None => short_addr(addr),
                };
                self.peers.push(Peer {
                    desc: *desc,
                    display_name,
                    connected: false,
                });
                if self.selected_peer.is_none() {
                    self.selected_peer = Some(0);
                }
                self.status = format!("Discovered peer: {}", label);
            }

            ChatEvent::PeerConnected { peer_address } => {
                if let Some(p) = self
                    .peers
                    .iter_mut()
                    .find(|p| p.desc.address_hash == peer_address)
                {
                    p.connected = true;
                }
                self.status = format!("Connected to {}", short_addr(peer_address));
            }

            ChatEvent::PeerDisconnected { peer_address } => {
                // Remove the peer from the list entirely — they will reappear
                // when they come back online and re-announce.
                let before = self.peers.len();
                self.peers.retain(|p| p.desc.address_hash != peer_address);
                // Fix up the selection index so it doesn't point past the end.
                if self.peers.is_empty() {
                    self.selected_peer = None;
                } else if let Some(idx) = self.selected_peer {
                    if idx >= self.peers.len() {
                        self.selected_peer = Some(self.peers.len() - 1);
                    }
                }
                if self.peers.len() < before {
                    self.status = format!("Peer {} disconnected", short_addr(peer_address));
                }
            }

            ChatEvent::MessageReceived { message } => {
                let addr = message.sender;
                // Ensure peer entry exists for inbound contacts we haven't seen before.
                if !self.peers.iter().any(|p| p.desc.address_hash == addr) {
                    // We can't reconstruct a full DestinationDesc from just an
                    // address hash, so we skip adding a peer entry here and just
                    // record the message.
                    self.status = format!("Message from unknown peer {}", short_addr(addr));
                } else {
                    self.status = format!("New message from {}", short_addr(addr));
                }
                let entry = MessageEntry::from_chat_msg(&message, self.handle.own_address);
                self.push_message(addr, entry);
            }
        }
    }

    fn push_message(&mut self, addr: AddressHash, entry: MessageEntry) {
        let conv = self.conversations.entry(addr).or_default();
        conv.push_back(entry);
        if conv.len() > MAX_MESSAGES {
            conv.pop_front();
        }
    }

    /// Messages for the currently selected peer (empty if none selected).
    ///
    /// `VecDeque` can wrap internally; this collects both halves so callers
    /// always see the full conversation.
    pub fn current_messages(&self) -> Vec<&MessageEntry> {
        self.selected_peer_address()
            .and_then(|a| self.conversations.get(&a))
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn short_addr(addr: AddressHash) -> String {
    let hex = addr.to_hex_string();
    format!("{}…{}", &hex[..6], &hex[hex.len() - 4..])
}
