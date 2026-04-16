//! Generic status UI primitives for `reticulum-node`.
//!
//! This module is intentionally board-agnostic:
//! - It provides a compact status data model suitable for embedded nodes.
//! - It renders a Meshtastic-inspired "status dashboard" chrome with `ratatui`.
//! - It leaves display backend/event-loop plumbing to board crates (for example
//!   via `mousefood` on small displays).
//!
//! Board crates are expected to:
//! 1. Implement [`StatusDataSource`] to provide live status snapshots.
//! 2. Implement one or more [`StatusScreen`] renderers for board-specific pages.
//! 3. Implement [`UiInput`] and [`StatusScreenSet`] to connect hardware input
//!    to page rendering, then call [`StatusUi::render_set`] each frame.

use core::fmt::Write;

use heapless::{String, Vec};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs};
use ratatui::Frame;

/// Re-exported so board crates can bind to the same `mousefood` version.
pub use mousefood;
/// Re-exported rectangle type to avoid board crates depending on `ratatui` directly.
pub use ratatui::layout::Rect as UiRect;
/// Re-exported frame type to avoid board crates depending on `ratatui` directly.
pub use ratatui::Frame as UiFrame;

/// Logical display orientation used by a UI panel profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayOrientation {
    /// Native portrait orientation.
    Portrait,
    /// Rotated into landscape orientation.
    Landscape,
}

/// Generic profile for small embedded UI panels.
pub trait DisplayProfile {
    /// Human-readable profile name.
    const NAME: &'static str;
    /// Display controller (for example `"ST7789V2"`).
    const CONTROLLER: &'static str;
    /// Active pixel width.
    const WIDTH: u16;
    /// Active pixel height.
    const HEIGHT: u16;
    /// Controller x offset into display RAM.
    const OFFSET_X: u16;
    /// Controller y offset into display RAM.
    const OFFSET_Y: u16;
    /// SPI clock used by the panel.
    const SPI_HZ: u32;
    /// Panel orientation.
    const ORIENTATION: DisplayOrientation;
}

/// GPIO address in `(port, pin)` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioPin {
    /// Nordic GPIO port number.
    pub port: u8,
    /// Pin number inside the GPIO port.
    pub pin: u8,
}

impl GpioPin {
    /// Construct a pin identifier.
    pub const fn new(port: u8, pin: u8) -> Self {
        Self { port, pin }
    }
}

/// Board wiring for a SPI-attached TFT panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiDisplayPins {
    /// SPI clock line.
    pub sck: GpioPin,
    /// SPI MOSI line.
    pub mosi: GpioPin,
    /// Chip select (NSS).
    pub cs: GpioPin,
    /// Data/command selection line.
    pub dc: GpioPin,
    /// Panel reset line.
    pub rst: GpioPin,
    /// Backlight control line.
    pub backlight: GpioPin,
}

/// Board-provided input actions for page navigation/control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// Select next tab/page.
    NextTab,
    /// Select previous tab/page.
    PreviousTab,
    /// Select/confirm in the current page.
    Select,
    /// Return/back action.
    Back,
    /// Trigger a manual refresh.
    Refresh,
}

/// Input source abstraction (buttons, encoder, touch, etc.).
pub trait UiInput {
    /// Platform-specific error type.
    type Error;

    /// Poll one input action, if available.
    fn poll_action(&mut self) -> Result<Option<UiAction>, Self::Error>;
}

/// Collection of board-defined status pages.
pub trait StatusScreenSet<const N_IFACES: usize, const N_EVENTS: usize> {
    /// Number of screens available.
    fn len(&self) -> usize;

    /// Whether this collection has no screens.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Screen title used for tab rendering.
    fn title(&self, index: usize) -> &'static str;

    /// Render the selected screen.
    fn render(
        &mut self,
        index: usize,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    );
}

/// Generic, fixed-capacity status snapshot supplied by a board crate.
#[derive(Debug, Clone)]
pub struct StatusSnapshot<const N_IFACES: usize, const N_EVENTS: usize> {
    /// Human-readable node name or role.
    pub node_name: String<32>,
    /// Node uptime in whole seconds.
    pub uptime_seconds: u64,
    /// Routing table size.
    pub path_count: usize,
    /// Dedup cache occupancy.
    pub seen_count: usize,
    /// Optional battery percentage from 0 to 100.
    pub battery_percent: Option<u8>,
    /// Optional receive signal estimate (for the active link).
    pub signal_dbm: Option<i16>,
    /// Aggregate RX packet counter.
    pub rx_packets: u32,
    /// Aggregate TX packet counter.
    pub tx_packets: u32,
    /// Per-interface summaries for the "Interfaces" screen.
    pub interfaces: Vec<InterfaceStatus, N_IFACES>,
    /// Recent event strings for quick diagnostics.
    pub events: Vec<String<64>, N_EVENTS>,
}

impl<const N_IFACES: usize, const N_EVENTS: usize> Default for StatusSnapshot<N_IFACES, N_EVENTS> {
    fn default() -> Self {
        Self {
            node_name: String::new(),
            uptime_seconds: 0,
            path_count: 0,
            seen_count: 0,
            battery_percent: None,
            signal_dbm: None,
            rx_packets: 0,
            tx_packets: 0,
            interfaces: Vec::new(),
            events: Vec::new(),
        }
    }
}

/// Compact per-interface status line.
#[derive(Debug, Clone, Default)]
pub struct InterfaceStatus {
    /// Interface label (for example `"LoRa"` or `"USB"`).
    pub name: String<16>,
    /// Preformatted summary (for example `"RX 120 / TX 98 / -89 dBm"`).
    pub summary: String<64>,
}

/// Data source used by the generic UI shell.
pub trait StatusDataSource<const N_IFACES: usize, const N_EVENTS: usize> {
    /// Read current node status into a fixed-capacity snapshot.
    fn snapshot(&self) -> StatusSnapshot<N_IFACES, N_EVENTS>;
}

/// A board-defined status page that renders into the main content area.
pub trait StatusScreen<const N_IFACES: usize, const N_EVENTS: usize> {
    /// Short tab title (for example `"Overview"`).
    fn title(&self) -> &'static str;

    /// Render screen content.
    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    );
}

/// Shared UI shell (header tabs + footer telemetry), inspired by Meshtastic.
#[derive(Debug, Clone)]
pub struct StatusUi {
    selected_tab: usize,
}

impl Default for StatusUi {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusUi {
    /// Create a new UI shell with tab index set to `0`.
    pub const fn new() -> Self {
        Self { selected_tab: 0 }
    }

    /// Return the currently selected tab index.
    pub const fn selected_tab(&self) -> usize {
        self.selected_tab
    }

    /// Select the next tab in a circular list.
    pub fn next_tab(&mut self, total_tabs: usize) {
        if total_tabs == 0 {
            self.selected_tab = 0;
            return;
        }
        self.selected_tab = (self.selected_tab + 1) % total_tabs;
    }

    /// Select the previous tab in a circular list.
    pub fn previous_tab(&mut self, total_tabs: usize) {
        if total_tabs == 0 {
            self.selected_tab = 0;
            return;
        }
        self.selected_tab = if self.selected_tab == 0 {
            total_tabs - 1
        } else {
            self.selected_tab - 1
        };
    }

    /// Force a specific tab index; out-of-range values clamp to `0`.
    pub fn set_tab(&mut self, index: usize, total_tabs: usize) {
        self.selected_tab = if index < total_tabs { index } else { 0 };
    }

    /// Render chrome and one board-provided screen.
    ///
    /// `tab_titles` should contain all available screens so the shell can draw
    /// the tab strip, while `screen` is the active page chosen by the board.
    pub fn render<S, const N_IFACES: usize, const N_EVENTS: usize>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
        tab_titles: &[&'static str],
        screen: &mut S,
    ) where
        S: StatusScreen<N_IFACES, N_EVENTS>,
    {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        self.render_tabs(frame, layout[0], tab_titles);
        screen.render(frame, layout[1], snapshot);
        self.render_footer(frame, layout[2], snapshot);
    }

    /// Render chrome and the currently selected page from a screen set.
    pub fn render_set<Set, const N_IFACES: usize, const N_EVENTS: usize>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
        screens: &mut Set,
    ) where
        Set: StatusScreenSet<N_IFACES, N_EVENTS>,
    {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        let tab_count = screens.len();
        if tab_count == 0 {
            self.render_tabs(frame, layout[0], &[]);
            let empty = Paragraph::new("no screens configured")
                .block(Block::default().title("status").borders(Borders::ALL));
            frame.render_widget(empty, layout[1]);
            self.render_footer(frame, layout[2], snapshot);
            return;
        }

        let mut titles: Vec<&'static str, 16> = Vec::new();
        for idx in 0..tab_count.min(16) {
            let _ = titles.push(screens.title(idx));
        }

        self.render_tabs(frame, layout[0], titles.as_slice());
        screens.render(self.selected_tab % tab_count, frame, layout[1], snapshot);
        self.render_footer(frame, layout[2], snapshot);
    }

    /// Apply an input action to update tab selection.
    pub fn handle_action(&mut self, action: UiAction, total_tabs: usize) {
        match action {
            UiAction::NextTab => self.next_tab(total_tabs),
            UiAction::PreviousTab => self.previous_tab(total_tabs),
            UiAction::Select | UiAction::Back | UiAction::Refresh => {}
        }
    }

    fn render_tabs(&self, frame: &mut Frame<'_>, area: Rect, tab_titles: &[&'static str]) {
        let selected = if tab_titles.is_empty() {
            0
        } else {
            self.selected_tab % tab_titles.len()
        };
        let tabs = Tabs::new(tab_titles.iter().copied())
            .block(
                Block::default()
                    .title(" Reticulum Node ")
                    .borders(Borders::ALL),
            )
            .select(selected)
            .style(Style::default().fg(Color::White))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_widget(tabs, area);
    }

    fn render_footer<const N_IFACES: usize, const N_EVENTS: usize>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    ) {
        let sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(18),
                Constraint::Length(14),
                Constraint::Min(10),
            ])
            .split(area);

        let battery = snapshot.battery_percent.unwrap_or(0).min(100);
        let battery_label = if snapshot.battery_percent.is_some() {
            "battery"
        } else {
            "battery n/a"
        };

        let gauge = Gauge::default()
            .block(Block::default().title(battery_label).borders(Borders::ALL))
            .gauge_style(Style::default().fg(Color::Green))
            .percent(u16::from(battery));
        frame.render_widget(gauge, sections[0]);

        let mut signal = String::<32>::new();
        if let Some(dbm) = snapshot.signal_dbm {
            let _ = write!(signal, "signal {} dBm", dbm);
        } else {
            let _ = signal.push_str("signal n/a");
        }
        let signal_widget = Paragraph::new(signal.as_str())
            .block(Block::default().title("radio").borders(Borders::ALL));
        frame.render_widget(signal_widget, sections[1]);

        let mut counters = String::<64>::new();
        let _ = write!(
            counters,
            "rx {}  tx {}  paths {}",
            snapshot.rx_packets, snapshot.tx_packets, snapshot.path_count
        );
        let counters_widget = Paragraph::new(counters.as_str())
            .block(Block::default().title("mesh").borders(Borders::ALL));
        frame.render_widget(counters_widget, sections[2]);
    }
}

/// Default "Overview" screen suitable for constrained displays.
#[derive(Debug, Clone, Default)]
pub struct OverviewScreen;

impl<const N_IFACES: usize, const N_EVENTS: usize> StatusScreen<N_IFACES, N_EVENTS>
    for OverviewScreen
{
    fn title(&self) -> &'static str {
        "Overview"
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    ) {
        let body = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(4)])
            .split(area);

        let mut l1 = String::<64>::new();
        let mut l2 = String::<64>::new();
        let mut l3 = String::<64>::new();
        let mut l4 = String::<64>::new();

        let _ = write!(l1, "node: {}", snapshot.node_name.as_str());
        let _ = write!(
            l2,
            "uptime: {}",
            format_uptime(snapshot.uptime_seconds).as_str()
        );
        let _ = write!(l3, "dedup cache: {}", snapshot.seen_count);
        let _ = write!(l4, "ifaces: {}", snapshot.interfaces.len());

        let stats = List::new([
            ListItem::new(Line::from(l1.as_str())),
            ListItem::new(Line::from(l2.as_str())),
            ListItem::new(Line::from(l3.as_str())),
            ListItem::new(Line::from(l4.as_str())),
        ])
        .block(Block::default().title("status").borders(Borders::ALL));
        frame.render_widget(stats, body[0]);

        let events = if snapshot.events.is_empty() {
            List::new([ListItem::new(Line::from("no recent events"))])
        } else {
            List::new(
                snapshot
                    .events
                    .iter()
                    .map(|event| ListItem::new(Line::from(event.as_str()))),
            )
        }
        .block(Block::default().title("recent").borders(Borders::ALL));
        frame.render_widget(events, body[1]);
    }
}

/// Default "Interfaces" screen using preformatted per-interface summaries.
#[derive(Debug, Clone, Default)]
pub struct InterfacesScreen;

impl<const N_IFACES: usize, const N_EVENTS: usize> StatusScreen<N_IFACES, N_EVENTS>
    for InterfacesScreen
{
    fn title(&self) -> &'static str {
        "Interfaces"
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        snapshot: &StatusSnapshot<N_IFACES, N_EVENTS>,
    ) {
        let rows = if snapshot.interfaces.is_empty() {
            List::new([ListItem::new(Line::from("no interfaces"))])
        } else {
            List::new(
                snapshot
                    .interfaces
                    .iter()
                    .map(|iface| ListItem::new(Line::from(iface.summary.as_str()))),
            )
        }
        .block(Block::default().title("links").borders(Borders::ALL));

        frame.render_widget(rows, area);
    }
}

fn format_uptime(seconds: u64) -> String<32> {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let mut out = String::<32>::new();
    if days > 0 {
        let _ = write!(out, "{}d {:02}h {:02}m", days, hours, minutes);
    } else {
        let _ = write!(out, "{:02}h {:02}m", hours, minutes);
    }
    out
}
