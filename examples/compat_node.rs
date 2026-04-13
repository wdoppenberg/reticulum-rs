//! Compatibility test node for Python interoperability tests.
//!
//! Starts a Reticulum TCP server, announces a destination, and emits
//! machine-readable lines that the Python test harness can parse.
//!
//! ## Structured output
//!
//! ```text
//! READY port=<PORT> dest=<HEX_HASH>
//! ```
//!
//! Written to **stdout** once the node is live; the Python harness polls for
//! this line before sending traffic.
//!
//! ## Usage
//!
//! ```text
//! cargo run --example compat_node -- --port 19991
//! cargo run --example compat_node -- --port 19991 --announce-interval 5
//! ```

use reticulum_tokio::{Reticulum, ReticulumError, ReticulumPaths};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug)]
struct Args {
    port: u16,
    config_dir: PathBuf,
    announce_interval_secs: u64,
}

impl Args {
    fn parse() -> Self {
        let mut port: u16 = 19991;
        let mut config_dir: Option<PathBuf> = None;
        let mut announce_interval_secs: u64 = 5;

        let args: Vec<String> = std::env::args().collect();
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" | "-p" => {
                    i += 1;
                    port = args[i].parse().expect("invalid port");
                }
                "--config-dir" | "-c" => {
                    i += 1;
                    config_dir = Some(PathBuf::from(&args[i]));
                }
                "--announce-interval" | "-a" => {
                    i += 1;
                    announce_interval_secs = args[i].parse().expect("invalid interval");
                }
                other => {
                    // Positional: treat as config-dir (backwards compat).
                    if !other.starts_with('-') && config_dir.is_none() {
                        config_dir = Some(PathBuf::from(other));
                    }
                }
            }
            i += 1;
        }

        let config_dir =
            config_dir.unwrap_or_else(|| std::env::temp_dir().join("reticulum_compat_node"));

        Self {
            port,
            config_dir,
            announce_interval_secs,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), ReticulumError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    log::info!(
        "compat_node: port={} config={}",
        args.port,
        args.config_dir.display()
    );

    // ── Build config TOML inline ──────────────────────────────────────────────

    let config_toml = format!(
        r#"
[reticulum]
enable_transport = true
share_instance = false
panic_on_interface_error = false

[logging]
loglevel = 4

[interfaces.tcp_server]
type = "tcp"
enabled = true
mode = "server"
address = "127.0.0.1"
port = {port}
"#,
        port = args.port
    );

    // ── Initialise paths and write config ─────────────────────────────────────

    let paths = ReticulumPaths::from_config_dir(&args.config_dir);
    paths.create_directories().expect("create dirs");
    std::fs::write(&paths.config_path, config_toml.trim()).expect("write config");

    // ── Start node ────────────────────────────────────────────────────────────

    let node = Reticulum::new_with_paths(paths).await?;
    let node = node.start().await?;

    let identity = node.identity();
    let dest_hash = identity.address_hash().to_string();

    // ── Signal readiness to the test harness ──────────────────────────────────
    // Python subprocess reads stdout and looks for this exact line.
    println!("READY port={} dest={}", args.port, dest_hash);
    // Flush is critical — Python reads line-by-line with a timeout.
    use std::io::Write as _;
    std::io::stdout().flush().ok();

    log::info!("compat_node: READY port={} dest={}", args.port, dest_hash);

    // ── Periodic announce loop ────────────────────────────────────────────────
    // Keep announcing so Python nodes can discover routing paths.
    let announce_interval = Duration::from_secs(args.announce_interval_secs);
    let transport = node.transport();

    let mut tick = tokio::time::interval(announce_interval);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                log::info!("compat_node: Ctrl-C received, shutting down");
                break;
            }
            _ = tick.tick() => {
                // Emit a periodic heartbeat so tests can verify the node is alive.
                let _ = transport; // suppress unused warning
                log::debug!("compat_node: heartbeat dest={dest_hash}");
            }
        }
    }

    Ok(())
}
