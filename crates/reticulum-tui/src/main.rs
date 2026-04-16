//! `reticulum-tui` — Terminal chat client for the Reticulum Network Stack.
//!
//! # Usage
//!
//! ```text
//! reticulum-tui [--config-dir <path>] [--name <display-name>] [--tmp]
//! ```
//!
//! If no `--config-dir` is given the node uses the default Reticulum config
//! location (`~/.reticulum`).  The node will load or generate an identity,
//! start the transport layer, register a `reticulum_chat.text` destination,
//! and announce itself on all configured interfaces.
//!
//! Pass `--tmp` to create a throwaway instance under `/tmp/.reticulum/<id>/`.
//! This is useful for running two chat nodes on the same machine to test them.
//!
//! # Key bindings
//!
//! | Key | Context | Action |
//! |-----|---------|--------|
//! | `Tab` | Peers / Input | Toggle focus between peer list and message input |
//! | `↑` / `k` | Peers | Select previous peer |
//! | `↓` / `j` | Peers | Select next peer |
//! | `c` / `Enter` | Peers | Connect to selected peer |
//! | `s` | Peers | Open settings screen |
//! | `Enter` | Input | Send message |
//! | `Backspace` | Input | Delete last character |
//! | `Esc` / `Tab` | Input | Return to peer list |
//! | `↑` / `k` | Settings | Select previous setting |
//! | `↓` / `j` | Settings | Select next setting |
//! | `Space` / `Enter` | Settings | Toggle boolean / increment number |
//! | `+` / `→` | Settings | Increment number |
//! | `-` / `←` | Settings | Decrement number |
//! | `a` | Settings | Toggle AutoInterface |
//! | `w` | Settings | Write config to disk |
//! | `Esc` / `q` | Settings | Return to peer list |
//! | `Esc` / `q` | Peers | Quit |
//! | `Ctrl-C` | Any | Quit |

mod app;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use reticulum_core::identity::PrivateIdentity;
use reticulum_tokio::config::{
    AutoInterfaceConfig, Config, InterfaceConfig, ReticulumConfig, TcpInterfaceConfig,
};
use reticulum_tokio::{Reticulum, ReticulumPaths};

use app::{App, Focus};

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (config_dir_arg, display_name, is_tmp) = parse_args();

    init_logger();

    // ── Determine config directory ────────────────────────────────────────────

    let config_dir: PathBuf = if is_tmp {
        create_tmp_dir()?
    } else {
        config_dir_arg.unwrap_or_else(ReticulumPaths::default_config_dir)
    };

    let paths = ReticulumPaths::from_config_dir(&config_dir);

    // ── Pre-flight: ensure config is sane before starting the node ────────────

    // Create directories + default config if this is a first run.
    paths.create_directories()?;

    // For `--tmp`, probe which loopback ports are free and write a tailored
    // TCP-loopback config.  AutoInterface cannot connect two processes on the
    // same host (they share the same link-local IPv6 address and each silently
    // drops the other's multicast probes as self-loopback).
    if is_tmp {
        let (server_port, client_port) = probe_tmp_ports();
        let config = build_tmp_config(server_port, client_port);
        config
            .to_file(&paths.config_path)
            .map_err(|e| anyhow::anyhow!("Failed to write tmp config: {}", e))?;
        log::info!(
            "tmp instance: TCP server on :{server_port}, client to :{client_port}"
        );
    } else if !paths.config_path.exists() {
        let toml = Config::generate_default_toml();
        std::fs::write(&paths.config_path, toml)?;
    }

    // Load the config so we can inspect (and fix) it before starting the node.
    let mut config = Config::from_file(&paths.config_path)
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

    // Detect issues that would prevent the chat app from working.
    let issues = detect_issues(&config);

    if !issues.is_empty() {
        // Build proposed auto-fixes description.
        let fixes = describe_fixes(&config, &issues);

        // Run the blocking wizard TUI.
        let apply = run_preflight_wizard(&paths.config_path, &issues, &fixes)?;

        if !apply {
            println!("Exiting. Edit {} and restart.", paths.config_path.display());
            return Ok(());
        }

        // Apply fixes in memory and save.
        apply_fixes(&mut config);
        config
            .to_file(&paths.config_path)
            .map_err(|e| anyhow::anyhow!("Failed to save config: {}", e))?;
    }

    // ── Set up the Reticulum node ─────────────────────────────────────────────

    let node = Reticulum::new(Some(config_dir)).await?;

    let node = node.start().await?;

    let transport = node
        .transport()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Transport layer failed to start. Check the log at {} for details.",
                std::env::temp_dir().join("reticulum-tui.log").display()
            )
        })?
        .clone();

    let config = node.config().clone();
    let paths = node.paths().clone();

    // Load or create a *chat* identity (separate from the node's network identity).
    let chat_identity = load_or_create_chat_identity(&paths)?;

    // Start the chat node.
    let handle = reticulum_chat::start(transport, chat_identity, display_name)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let mut events_rx = handle.subscribe();
    let mut app = App::new(handle, config, paths, is_tmp);

    // ── Set up the terminal ───────────────────────────────────────────────────

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ── Main loop ─────────────────────────────────────────────────────────────

    let tick = Duration::from_millis(50);

    loop {
        // Drain pending chat events (non-blocking).
        loop {
            match events_rx.try_recv() {
                Ok(ev) => app.process_event(ev),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    app.should_quit = true;
                    break;
                }
            }
        }

        // Render.
        terminal.draw(|f| ui::draw(f, &app))?;

        // Poll for a key event within one tick.
        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                handle_key(&mut app, key.code, key.modifiers).await;
            }
        }

        if app.should_quit {
            break;
        }
    }

    // ── Restore terminal ──────────────────────────────────────────────────────

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    println!("Bye.");
    Ok(())
}

// ── Key event handling ────────────────────────────────────────────────────────

async fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // Ctrl-C always quits regardless of focus.
    if let KeyCode::Char('c') = code {
        if modifiers.contains(KeyModifiers::CONTROL) {
            app.should_quit = true;
            return;
        }
    }

    match app.focus {
        Focus::Peers => match code {
            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
            KeyCode::Char('c') | KeyCode::Enter => app.connect_selected().await,
            KeyCode::Down | KeyCode::Char('j') => app.select_next_peer(),
            KeyCode::Up | KeyCode::Char('k') => app.select_prev_peer(),
            KeyCode::Tab => app.focus = Focus::Input,
            KeyCode::Char('s') => app.focus = Focus::Settings,
            _ => {}
        },
        Focus::Input => match code {
            KeyCode::Esc | KeyCode::Tab => app.focus = Focus::Peers,
            KeyCode::Enter => app.send_current_input().await,
            KeyCode::Backspace => app.pop_char(),
            KeyCode::Char('s') if modifiers.contains(KeyModifiers::ALT) => {
                app.focus = Focus::Settings;
            }
            KeyCode::Char(c) => app.push_char(c),
            _ => {}
        },
        Focus::Settings => match code {
            KeyCode::Esc | KeyCode::Char('q') => app.focus = Focus::Peers,
            KeyCode::Up | KeyCode::Char('k') => app.settings_nav_up(),
            KeyCode::Down | KeyCode::Char('j') => app.settings_nav_down(),
            KeyCode::Char(' ') | KeyCode::Enter => app.settings_activate(),
            KeyCode::Char('+') | KeyCode::Right => app.settings_activate(),
            KeyCode::Char('-') | KeyCode::Left => app.settings_decrement(),
            KeyCode::Char('a') => app.settings_toggle_auto_iface(),
            KeyCode::Char('w') => app.settings_save(),
            _ => {}
        },
    }
}

// ── Pre-flight helpers ────────────────────────────────────────────────────────

/// Return a list of human-readable issues that would prevent chat from working.
fn detect_issues(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();

    if !config.reticulum.enable_transport {
        issues.push("Transport is disabled — required for chat to work".to_string());
    }

    if config.interfaces.is_empty() {
        issues.push("No network interfaces configured — peers cannot be discovered".to_string());
    } else {
        let all_disabled = config.interfaces.values().all(|iface| match iface {
            InterfaceConfig::Auto(a) => !a.enabled,
            InterfaceConfig::Tcp(t) => !t.enabled,
            InterfaceConfig::Udp(u) => !u.enabled,
            InterfaceConfig::Serial(s) => !s.enabled,
            InterfaceConfig::I2P(i) => !i.enabled,
        });
        if all_disabled {
            issues.push("All network interfaces are disabled — peers cannot be discovered".to_string());
        }
    }

    issues
}

/// Return human-readable descriptions of what will be changed to fix `issues`.
fn describe_fixes(config: &Config, issues: &[String]) -> Vec<String> {
    let mut fixes = Vec::new();

    for issue in issues {
        if issue.contains("Transport") {
            fixes.push("Set  enable_transport = true".to_string());
        }
        if issue.contains("interface") {
            if config.interfaces.is_empty() {
                fixes.push("Add  [interfaces.auto]  type = \"auto\"  enabled = true  (local multicast)".to_string());
            } else {
                fixes.push("Enable all currently-configured interfaces".to_string());
            }
        }
    }

    fixes
}

/// Mutate `config` in-place to fix all detected issues.
fn apply_fixes(config: &mut Config) {
    config.reticulum.enable_transport = true;

    if config.interfaces.is_empty() {
        config.interfaces.insert(
            "auto".to_string(),
            InterfaceConfig::Auto(AutoInterfaceConfig {
                enabled: true,
                group: None,
                discovery_port: None,
                data_port: None,
            }),
        );
    } else {
        // Enable any disabled interfaces.
        for iface in config.interfaces.values_mut() {
            match iface {
                InterfaceConfig::Auto(a) => { a.enabled = true; }
                InterfaceConfig::Tcp(t) => { t.enabled = true; }
                InterfaceConfig::Udp(u) => { u.enabled = true; }
                InterfaceConfig::Serial(s) => { s.enabled = true; }
                InterfaceConfig::I2P(i) => { i.enabled = true; }
            }
        }
    }
}

/// Run a blocking TUI wizard that shows issues and proposed fixes, returning
/// `true` if the user chose to apply them.
fn run_preflight_wizard(
    config_path: &std::path::Path,
    issues: &[String],
    fixes: &[String],
) -> anyhow::Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        terminal.draw(|f| ui::draw_wizard(f, config_path, issues, fixes))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => break true,
                    KeyCode::Char('n') | KeyCode::Esc => break false,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break false;
                    }
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;

    Ok(result)
}

// ── Tmp-instance helpers ──────────────────────────────────────────────────────

/// The loopback TCP ports used by `--tmp` instances to discover each other.
///
/// Two fixed ports let us use a simple "first-come first-served" probe to
/// assign roles without any external coordination:
///   • first instance  → server on PORT_A, client to PORT_B
///   • second instance → server on PORT_B, client to PORT_A
const TMP_PORT_A: u16 = 19998;
const TMP_PORT_B: u16 = 19999;

/// Create a fresh, uniquely-named directory under `/tmp/.reticulum/` for an
/// ephemeral test instance.
fn create_tmp_dir() -> anyhow::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_micros();
    let pid = std::process::id();
    let name = format!("{:08x}{:06x}", pid, micros);
    let dir = PathBuf::from("/tmp/.reticulum").join(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Probe loopback to decide which TCP port this instance should serve on and
/// which port to connect to as a client.
///
/// AutoInterface cannot link two processes on the same host because they share
/// the same link-local IPv6 address and filter each other's multicast probes as
/// self-loopback.  TCP loopback has no such restriction.
///
/// Returns `(server_port, client_port)`.
fn probe_tmp_ports() -> (u16, u16) {
    // A successful bind means PORT_A is currently free — claim it.
    // The listener is dropped immediately; Reticulum will re-bind it when it
    // starts the TCP server interface.  There is a tiny TOCTOU window, but for
    // a local testing tool this is acceptable.
    let can_take_a = std::net::TcpListener::bind(("127.0.0.1", TMP_PORT_A)).is_ok();
    if can_take_a {
        (TMP_PORT_A, TMP_PORT_B)
    } else {
        (TMP_PORT_B, TMP_PORT_A)
    }
}

/// Build a Config for a `--tmp` instance that communicates with a peer via TCP
/// loopback rather than AutoInterface.
fn build_tmp_config(server_port: u16, client_port: u16) -> Config {
    use std::collections::HashMap;

    let mut interfaces = HashMap::new();

    interfaces.insert(
        "tmp_server".to_string(),
        InterfaceConfig::Tcp(TcpInterfaceConfig {
            enabled: true,
            mode: "server".to_string(),
            address: "127.0.0.1".to_string(),
            port: server_port,
            bitrate: None,
            outbound: true,
        }),
    );
    interfaces.insert(
        "tmp_client".to_string(),
        InterfaceConfig::Tcp(TcpInterfaceConfig {
            enabled: true,
            mode: "client".to_string(),
            address: "127.0.0.1".to_string(),
            port: client_port,
            bitrate: None,
            outbound: true,
        }),
    );

    Config {
        reticulum: ReticulumConfig {
            enable_transport: true,
            // Disable daemon sharing so two tmp instances don't fight over the
            // same local socket.
            share_instance: false,
            ..ReticulumConfig::default()
        },
        logging: reticulum_tokio::config::LoggingConfig::default(),
        interfaces,
    }
}

// ── CLI argument parsing ──────────────────────────────────────────────────────

fn parse_args() -> (Option<PathBuf>, Option<String>, bool) {
    let mut args = std::env::args().skip(1);
    let mut config_dir: Option<PathBuf> = None;
    let mut display_name: Option<String> = None;
    let mut is_tmp = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config-dir" | "-c" => {
                config_dir = args.next().map(PathBuf::from);
            }
            "--name" | "-n" => {
                display_name = args.next();
            }
            "--tmp" | "-T" => {
                is_tmp = true;
            }
            _ => {}
        }
    }

    (config_dir, display_name, is_tmp)
}

fn init_logger() {
    let log_path = std::env::temp_dir().join("reticulum-tui.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("reticulum_chat=debug,warn"),
    );
    if let Some(file) = file {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();
}

// ── Identity helpers ──────────────────────────────────────────────────────────

/// Load the chat identity from `<storage>/chat_identity`, creating it if absent.
fn load_or_create_chat_identity(paths: &ReticulumPaths) -> anyhow::Result<PrivateIdentity> {
    use reticulum_core::identity::PUBLIC_KEY_LENGTH;
    const KEY_BYTES: usize = PUBLIC_KEY_LENGTH * 2;

    let path = paths.storage_path.join("chat_identity");

    if path.exists() {
        let bytes = std::fs::read(&path)?;
        if bytes.len() < KEY_BYTES {
            anyhow::bail!("chat identity file is too short");
        }
        let hex: String = bytes[..KEY_BYTES]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        PrivateIdentity::new_from_hex_string(&hex)
            .map_err(|e| anyhow::anyhow!("failed to decode chat identity: {:?}", e))
    } else {
        let identity = PrivateIdentity::new_from_rand(rand_core::OsRng);
        let hex = identity.to_hex_string();
        let raw: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        std::fs::write(&path, &raw)?;
        Ok(identity)
    }
}
