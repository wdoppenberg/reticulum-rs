//! TUI rendering.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use reticulum_tokio::config::InterfaceConfig;

use crate::app::{App, Focus, MessageEntry};

/// Render the full UI.
pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    // Settings screen replaces the normal layout.
    if app.focus == Focus::Settings {
        draw_settings(f, app, area);
        return;
    }

    // Split into three rows: main body + status bar.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let main_area = rows[0];
    let status_area = rows[1];

    // Split main area: peer list (25%) | conversation (75%).
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(main_area);

    let peer_area = cols[0];
    let right_area = cols[1];

    // Split right side: messages | input box.
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(right_area);

    let messages_area = right_rows[0];
    let input_area = right_rows[1];

    draw_peer_list(f, app, peer_area);
    draw_messages(f, app, messages_area);
    draw_input(f, app, input_area);
    draw_status(f, app, status_area);
}

// ── Peer list ─────────────────────────────────────────────────────────────────

fn draw_peer_list(f: &mut Frame, app: &App, area: Rect) {
    let focus = app.focus == Focus::Peers;

    let items: Vec<ListItem> = app
        .peers
        .iter()
        .enumerate()
        .map(|(i, peer)| {
            let label = app.peer_label(i);
            let (prefix, style) = if peer.connected {
                ("● ", Style::default().fg(Color::Green))
            } else {
                ("○ ", Style::default().fg(Color::DarkGray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::raw(label),
            ]))
        })
        .collect();

    let border_style = if focus {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Peers ")
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(app.selected_peer);

    f.render_stateful_widget(list, area, &mut state);
}

// ── Message area ──────────────────────────────────────────────────────────────

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_peer_address() {
        Some(addr) => {
            let hex = addr.to_hex_string();
            format!(" {} ", &hex[..12])
        }
        None => " Conversation ".to_string(),
    };

    // Build lines from messages. We only show what fits.
    let inner_height = area.height.saturating_sub(2) as usize;
    let msgs = app.current_messages();

    // Build all lines first, then take the tail that fits.
    let mut all_lines: Vec<Line> = Vec::new();
    for msg in msgs {
        all_lines.push(format_message(msg));
    }

    let start = all_lines.len().saturating_sub(inner_height);
    let visible: Vec<Line> = all_lines[start..].to_vec();

    let para = Paragraph::new(visible)
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    f.render_widget(para, area);
}

fn format_message(msg: &MessageEntry) -> Line<'static> {
    let time = msg.time_str();
    let (arrow, name_style, msg_style) = if msg.outgoing {
        (
            "→",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(Color::White),
        )
    } else {
        (
            "←",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            Style::default(),
        )
    };

    let sender_hex = msg.sender.to_hex_string();
    let sender_label = format!(
        "{}…{}",
        &sender_hex[..6],
        &sender_hex[sender_hex.len() - 4..]
    );

    Line::from(vec![
        Span::styled(format!("[{}] ", time), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{} ", arrow), name_style),
        Span::styled(format!("<{}> ", sender_label), name_style),
        Span::styled(msg.content.clone(), msg_style),
    ])
}

// ── Input box ─────────────────────────────────────────────────────────────────

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let focus = app.focus == Focus::Input;

    let border_style = if focus {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let input_text = if focus {
        format!("{}█", app.input) // block cursor
    } else {
        app.input.clone()
    };

    let para = Paragraph::new(input_text).block(
        Block::default()
            .title(if focus {
                " Message (Enter to send) "
            } else {
                " Message (Tab to focus) "
            })
            .borders(Borders::ALL)
            .border_style(border_style),
    );

    f.render_widget(para, area);
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let own = app.handle.own_address.to_hex_string();
    let own_short = format!("{}…{}", &own[..8], &own[own.len() - 6..]);

    let tmp_tag = if app.is_tmp { " [TMP] " } else { "" };

    let left = Span::styled(
        format!(" {}{} ", tmp_tag, app.status),
        Style::default().fg(Color::White),
    );
    let right = Span::styled(
        format!(
            " [me: {}]  [Tab] focus  [c] connect  [s] settings  [q] quit ",
            own_short
        ),
        Style::default().fg(Color::DarkGray),
    );

    let line = Line::from(vec![left, right]);
    let para = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    f.render_widget(para, area);
}

// ── Settings screen ───────────────────────────────────────────────────────────

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    // Split: settings panel (top, fills space) + key-hint bar (bottom, 1 line).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let panel_area = rows[0];
    let hint_area = rows[1];

    let bool_span = |val: bool| -> Span<'static> {
        if val {
            Span::styled(
                "enabled ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("disabled", Style::default().fg(Color::Red))
        }
    };

    let cursor = app.settings_cursor;

    // Helper: row background for the cursor position.
    let row_style = |idx: usize| -> Style {
        if idx == cursor {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    let cursor_marker = |idx: usize| -> &'static str {
        if idx == cursor {
            "▶ "
        } else {
            "  "
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // ── Header ────────────────────────────────────────────────────────────────

    let tmp_note = if app.is_tmp {
        "  [TMP — ephemeral instance]"
    } else {
        ""
    };
    lines.push(Line::from(vec![
        Span::styled("Config  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}{}", app.config_path.display(), tmp_note),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(""));

    // ── [reticulum] section ───────────────────────────────────────────────────

    lines.push(Line::from(Span::styled(
        "[reticulum]",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    // Each bool entry: (cursor_idx, field_name, value, description)
    let bool_rows: &[(usize, &str, bool, &str)] = &[
        (
            0,
            "enable_transport              ",
            app.config.reticulum.enable_transport,
            "must be true for chat",
        ),
        (
            1,
            "share_instance                ",
            app.config.reticulum.share_instance,
            "share this node with other local processes",
        ),
        (
            2,
            "link_mtu_discovery            ",
            app.config.reticulum.link_mtu_discovery,
            "auto-detect link MTU",
        ),
        (
            3,
            "use_implicit_proof            ",
            app.config.reticulum.use_implicit_proof,
            "use implicit proofs",
        ),
        (
            4,
            "allow_probes                  ",
            app.config.reticulum.allow_probes,
            "respond to network probes",
        ),
        (
            5,
            "enable_remote_management      ",
            app.config.reticulum.enable_remote_management,
            "allow remote management",
        ),
        (
            6,
            "enable_discovery              ",
            app.config.reticulum.enable_discovery,
            "enable discovery subsystem",
        ),
        (
            7,
            "discover_interfaces           ",
            app.config.reticulum.discover_interfaces,
            "auto-discover network interfaces",
        ),
        (
            8,
            "autoconnect_discovered_ifs    ",
            app.config.reticulum.autoconnect_discovered_interfaces,
            "auto-connect to discovered interfaces",
        ),
        (
            9,
            "panic_on_interface_error      ",
            app.config.reticulum.panic_on_interface_error,
            "crash on interface errors",
        ),
    ];

    for &(idx, name, val, desc) in bool_rows {
        lines.push(Line::from(vec![
            Span::styled(cursor_marker(idx), Style::default().fg(Color::Cyan)),
            Span::styled(format!("  {}", name), row_style(idx).fg(Color::White)),
            bool_span(val),
            Span::styled(format!("  — {}", desc), row_style(idx).fg(Color::DarkGray)),
        ]));
    }

    // Port rows: (cursor_idx, field_name, value)
    let port_rows: &[(usize, &str, u16)] = &[
        (
            10,
            "shared_instance_port          ",
            app.config.reticulum.shared_instance_port,
        ),
        (
            11,
            "instance_control_port         ",
            app.config.reticulum.instance_control_port,
        ),
    ];

    for &(idx, name, val) in port_rows {
        lines.push(Line::from(vec![
            Span::styled(cursor_marker(idx), Style::default().fg(Color::Cyan)),
            Span::styled(format!("  {}", name), row_style(idx).fg(Color::White)),
            Span::styled(
                format!("{:<6}", val),
                row_style(idx).fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  — [+/-] to adjust", row_style(idx).fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));

    // ── [logging] section ─────────────────────────────────────────────────────

    lines.push(Line::from(Span::styled(
        "[logging]",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    let ll = app.config.logging.loglevel;
    let ll_desc = match ll {
        0 => "Critical",
        1 => "Error",
        2 => "Warning",
        3 => "Notice",
        4 => "Info",
        5 => "Verbose",
        6 => "Debug",
        7 => "Extreme",
        _ => "?",
    };
    lines.push(Line::from(vec![
        Span::styled(cursor_marker(12), Style::default().fg(Color::Cyan)),
        Span::styled(
            "  loglevel                      ",
            row_style(12).fg(Color::White),
        ),
        Span::styled(
            format!("{} ({})", ll, ll_desc),
            row_style(12).fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  — 0=Critical … 7=Extreme  [+/-] to adjust",
            row_style(12).fg(Color::DarkGray),
        ),
    ]));

    lines.push(Line::from(""));

    // ── [interfaces] section ──────────────────────────────────────────────────

    lines.push(Line::from(Span::styled(
        "[interfaces]",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    if app.config.interfaces.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none — press [a] to add AutoInterface)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (name, iface) in &app.config.interfaces {
            let (kind, enabled) = match iface {
                InterfaceConfig::Auto(a) => ("auto      ", a.enabled),
                InterfaceConfig::Tcp(t) => (
                    if t.mode == "server" {
                        "tcp/server"
                    } else {
                        "tcp/client"
                    },
                    t.enabled,
                ),
                InterfaceConfig::Udp(u) => ("udp       ", u.enabled),
                InterfaceConfig::Serial(s) => {
                    let _ = &s.port;
                    ("serial    ", s.enabled)
                }
                InterfaceConfig::I2P(i) => {
                    let _ = &i.sam_host;
                    ("i2p       ", i.enabled)
                }
            };

            let name_label = if name == "auto" {
                "[a] auto  ".to_string()
            } else {
                format!("    {:<8}", name)
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {} ", name_label),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(format!("{} ", kind), Style::default().fg(Color::White)),
                bool_span(enabled),
            ]));
        }
    }

    lines.push(Line::from(""));

    // ── Unsaved-changes note / status ─────────────────────────────────────────

    lines.push(Line::from(Span::styled(
        "Changes are in-memory until you press [w] to write them to disk.",
        Style::default().fg(Color::DarkGray),
    )));
    if !app.status.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::Yellow),
        )));
    }

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Settings ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(para, panel_area);

    // ── Hint bar ──────────────────────────────────────────────────────────────
    let hints = Paragraph::new(Line::from(vec![
        Span::styled(
            " [↑/↓ k/j] navigate  [Space/Enter] toggle  [+/-] adjust numbers  [a] toggle AutoInterface  [w] save  [Esc/q] back ",
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .style(Style::default().bg(Color::DarkGray));
    f.render_widget(hints, hint_area);
}

// ── Wizard screen (pre-flight) ────────────────────────────────────────────────

/// Draw the pre-flight setup wizard to the given frame.
///
/// `issues` are the detected problems; `fixes` are the proposed remedies.
/// `confirmed` drives the highlight on the action buttons.
pub fn draw_wizard(
    f: &mut Frame,
    config_path: &std::path::Path,
    issues: &[String],
    fixes: &[String],
) {
    let area = f.area();

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Welcome to reticulum-tui!",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::styled(" Config: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            config_path.display().to_string(),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Issues detected:",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    for issue in issues {
        lines.push(Line::from(vec![
            Span::styled("   ✗ ", Style::default().fg(Color::Red)),
            Span::styled(issue.clone(), Style::default().fg(Color::White)),
        ]));
    }

    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Proposed fixes:",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )));
    for fix in fixes {
        lines.push(Line::from(vec![
            Span::styled("   ✓ ", Style::default().fg(Color::Green)),
            Span::styled(fix.clone(), Style::default().fg(Color::White)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Apply these fixes, save config, and continue?",
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("   ", Style::default()),
        Span::styled(
            " [y / Enter]  Yes, fix and continue ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("    ", Style::default()),
        Span::styled(
            " [n / Esc]  Exit ",
            Style::default().fg(Color::White).bg(Color::DarkGray),
        ),
    ]));

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" First-Run Setup ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(para, area);
}
