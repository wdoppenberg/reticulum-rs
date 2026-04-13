use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use reticulum_core::identity::PrivateIdentity;

use crate::config::{Config, ConfigError, InterfaceConfig};
use crate::iface::auto::AutoInterface;
use crate::iface::i2p::I2PInterface;
use crate::iface::serial::SerialInterface;
use crate::iface::tcp_client::TcpClient;
use crate::iface::tcp_server::TcpServer;
use crate::iface::udp::UdpInterface;
use crate::iface::InterfaceManager;
use crate::transport::{Transport, TransportConfig};

// ── Typestate markers ─────────────────────────────────────────────────────────

mod private {
    pub trait Sealed {}
}

/// Marker: node constructed but [`start`](super::Reticulum::start) not yet called.
pub struct Unstarted;
/// Marker: node fully started; interfaces and transport are live.
pub struct Running;

impl private::Sealed for Unstarted {}
impl private::Sealed for Running {}

/// Sealed trait that prevents external code from inventing new node states.
pub trait NodeState: private::Sealed {}
impl NodeState for Unstarted {}
impl NodeState for Running {}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ReticulumError {
    Config(ConfigError),
    Io(std::io::Error),
    Transport(String),
    Interface(String),
    AlreadyInitialized,
}

impl From<ConfigError> for ReticulumError {
    fn from(e: ConfigError) -> Self {
        ReticulumError::Config(e)
    }
}

impl From<std::io::Error> for ReticulumError {
    fn from(e: std::io::Error) -> Self {
        ReticulumError::Io(e)
    }
}

impl std::fmt::Display for ReticulumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReticulumError::Config(e) => write!(f, "Configuration error: {}", e),
            ReticulumError::Io(e) => write!(f, "IO error: {}", e),
            ReticulumError::Transport(s) => write!(f, "Transport error: {}", s),
            ReticulumError::Interface(s) => write!(f, "Interface error: {}", s),
            ReticulumError::AlreadyInitialized => {
                write!(f, "Reticulum instance already initialized")
            }
        }
    }
}

impl std::error::Error for ReticulumError {}

// ── Network constants ─────────────────────────────────────────────────────────

pub mod constants {
    use std::time::Duration;

    pub const MTU: usize = 500;
    pub const LINK_MTU_DISCOVERY: bool = true;
    pub const ANNOUNCE_CAP: u8 = 2;
    pub const MINIMUM_BITRATE: u64 = 5;
    pub const DEFAULT_PER_HOP_TIMEOUT: u64 = 6;
    pub const TRUNCATED_HASHLENGTH: usize = 128;
    pub const HEADER_MINSIZE: usize = 2 + 1 + (TRUNCATED_HASHLENGTH / 8);
    pub const HEADER_MAXSIZE: usize = 2 + 1 + (TRUNCATED_HASHLENGTH / 8) * 2;
    pub const IFAC_MIN_SIZE: usize = 1;
    pub const MDU: usize = MTU - HEADER_MAXSIZE - IFAC_MIN_SIZE;
    pub const RESOURCE_CACHE: Duration = Duration::from_secs(24 * 60 * 60);
    pub const JOB_INTERVAL: Duration = Duration::from_secs(5 * 60);
    pub const CLEAN_INTERVAL: Duration = Duration::from_secs(15 * 60);
    pub const PERSIST_INTERVAL: Duration = Duration::from_secs(60 * 60 * 12);
    pub const GRACIOUS_PERSIST_INTERVAL: Duration = Duration::from_secs(60 * 5);
    pub const MAX_QUEUED_ANNOUNCES: usize = 16384;
    pub const QUEUED_ANNOUNCE_LIFE: Duration = Duration::from_secs(60 * 60 * 24);
}

// ── Paths ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReticulumPaths {
    pub config_dir: PathBuf,
    pub config_path: PathBuf,
    pub storage_path: PathBuf,
    pub cache_path: PathBuf,
    pub resource_path: PathBuf,
    pub identity_path: PathBuf,
    pub interface_path: PathBuf,
}

impl ReticulumPaths {
    pub fn from_config_dir<P: AsRef<Path>>(config_dir: P) -> Self {
        let config_dir = config_dir.as_ref().to_path_buf();
        Self {
            config_path: config_dir.join("config.toml"),
            storage_path: config_dir.join("storage"),
            cache_path: config_dir.join("storage").join("cache"),
            resource_path: config_dir.join("storage").join("resources"),
            identity_path: config_dir.join("storage").join("identities"),
            interface_path: config_dir.join("interfaces"),
            config_dir,
        }
    }

    pub fn default_config_dir() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            let etc_path = PathBuf::from("/etc/reticulum");
            let xdg_path = PathBuf::from(&home).join(".config").join("reticulum");
            let home_path = PathBuf::from(&home).join(".reticulum");
            if etc_path.join("config.toml").exists() {
                return etc_path;
            }
            if xdg_path.join("config.toml").exists() {
                return xdg_path;
            }
            home_path
        } else {
            PathBuf::from(".reticulum")
        }
    }

    pub fn create_directories(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.storage_path)?;
        std::fs::create_dir_all(&self.cache_path)?;
        std::fs::create_dir_all(&self.resource_path)?;
        std::fs::create_dir_all(&self.identity_path)?;
        std::fs::create_dir_all(&self.interface_path)?;
        Ok(())
    }
}

// ── Identity persistence ──────────────────────────────────────────────────────

fn load_or_create_identity(path: &Path) -> Result<PrivateIdentity, ReticulumError> {
    use reticulum_core::identity::PUBLIC_KEY_LENGTH;
    const KEY_BYTES: usize = PUBLIC_KEY_LENGTH * 2;

    if path.exists() {
        log::debug!("Loading network identity from {}", path.display());
        let bytes = std::fs::read(path)?;
        if bytes.len() < KEY_BYTES {
            return Err(ReticulumError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "identity file {} is too short ({} bytes, expected {})",
                    path.display(),
                    bytes.len(),
                    KEY_BYTES
                ),
            )));
        }
        let hex: String = bytes[..KEY_BYTES]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        let identity = PrivateIdentity::new_from_hex_string(&hex).map_err(|e| {
            ReticulumError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to decode identity: {:?}", e),
            ))
        })?;
        log::info!(
            "Loaded network identity {} from {}",
            identity.address_hash(),
            path.display()
        );
        Ok(identity)
    } else {
        log::info!("Generating new network identity");
        let identity = PrivateIdentity::new_from_rand(rand_core::OsRng);
        let hex = identity.to_hex_string();
        let raw: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        std::fs::write(path, &raw)?;
        log::info!(
            "New network identity {} saved to {}",
            identity.address_hash(),
            path.display()
        );
        Ok(identity)
    }
}

// ── Inner data ────────────────────────────────────────────────────────────────

/// All Reticulum runtime state.  `Drop` lives here so that `Reticulum<S>`
/// itself has no `Drop` impl, which allows `start()` to move fields out via
/// plain destructuring without any `unsafe` or `ManuallyDrop`.
struct ReticulumData {
    config: Config,
    paths: ReticulumPaths,
    /// The node's private identity — lives here and nowhere else.
    identity: PrivateIdentity,
    /// `None` until `start()` completes; always `Some` when `S = Running`
    /// (if transport is enabled in config).
    transport: Option<Arc<Transport>>,
    interface_manager: Arc<Mutex<InterfaceManager>>,
    cancellation_token: CancellationToken,
}

impl Drop for ReticulumData {
    fn drop(&mut self) {
        self.cancellation_token.cancel();
        log::debug!("Reticulum instance dropped");
    }
}

// ── Reticulum<S> ─────────────────────────────────────────────────────────────

/// Main Reticulum instance.
///
/// `S` tracks the lifecycle:
/// - [`Unstarted`]: constructed; no interfaces or transport running yet.
///   Only [`config`](Self::config), [`paths`](Self::paths) and
///   [`start`](Self::start) are accessible.
/// - [`Running`]: fully live. [`identity`](Self::identity),
///   [`transport`](Self::transport) and [`interface_manager`](Self::interface_manager)
///   become accessible. Calling these on an `Unstarted` node is a **compile
///   error**.
///
/// The private identity is owned exclusively by this struct. The transport
/// receives only the node's `AddressHash` — it never holds a reference to
/// the private key.
///
/// `Reticulum<S>` itself has no `Drop` impl; cleanup is handled by
/// [`ReticulumData`]'s `Drop`, which cancels all background tasks.
pub struct Reticulum<S: NodeState = Unstarted> {
    data: ReticulumData,
    _state: PhantomData<S>,
}

// ── Methods available in every state ─────────────────────────────────────────

impl<S: NodeState> Reticulum<S> {
    pub fn config(&self) -> &Config {
        &self.data.config
    }

    pub fn paths(&self) -> &ReticulumPaths {
        &self.data.paths
    }
}

// ── Unstarted: construction and start ────────────────────────────────────────

impl Reticulum<Unstarted> {
    /// Construct a node, loading or creating config/identity as needed.
    ///
    /// This does **not** start any interfaces or the transport.  Call
    /// [`start`](Self::start) to bring the node live.
    pub async fn new(config_dir: Option<PathBuf>) -> Result<Self, ReticulumError> {
        let paths = config_dir
            .map(ReticulumPaths::from_config_dir)
            .unwrap_or_else(|| {
                ReticulumPaths::from_config_dir(ReticulumPaths::default_config_dir())
            });
        Self::new_with_paths(paths).await
    }

    /// Construct a node with explicit path configuration.
    pub async fn new_with_paths(paths: ReticulumPaths) -> Result<Self, ReticulumError> {
        paths.create_directories()?;

        let config = if paths.config_path.exists() {
            log::info!("Loading configuration from {}", paths.config_path.display());
            Config::from_file(&paths.config_path)?
        } else {
            log::info!("Creating default configuration file");
            let config = Config::default_config();
            std::fs::write(&paths.config_path, Config::generate_default_toml())?;
            log::info!(
                "Default config file created at {}.",
                paths.config_path.display()
            );
            config
        };

        let identity_file = paths.storage_path.join("identity");
        let identity = if let Some(override_path) = &config.reticulum.network_identity {
            let full_path = if override_path.is_absolute() {
                override_path.clone()
            } else {
                paths.config_dir.join(override_path)
            };
            load_or_create_identity(&full_path)?
        } else {
            load_or_create_identity(&identity_file)?
        };

        Ok(Self {
            data: ReticulumData {
                config,
                paths,
                identity,
                transport: None,
                interface_manager: Arc::new(Mutex::new(InterfaceManager::new(256))),
                cancellation_token: CancellationToken::new(),
            },
            _state: PhantomData,
        })
    }

    /// Bring up interfaces and the transport, then return a `Reticulum<Running>`
    /// whose live-node methods are accessible.
    ///
    /// Consumes `self` so the unstarted handle cannot be used afterwards.
    ///
    /// The private identity is **never** passed to the transport; only the
    /// derived `AddressHash` is.
    pub async fn start(self) -> Result<Reticulum<Running>, ReticulumError> {
        log::info!("Starting Reticulum Network Stack");

        // Reticulum<S> has no Drop impl, so plain destructuring is allowed.
        let Reticulum { mut data, .. } = self;

        let node_address = *data.identity.address_hash();

        if data.config.reticulum.enable_transport {
            log::info!("Starting transport layer");
            let transport = Arc::new(Transport::new(TransportConfig::new(
                "main",
                node_address,
                true,
            )));
            // Wire interfaces into the transport's own InterfaceManager so
            // packets flow through the transport's internal receive loop.
            initialize_interfaces(&data.config, &transport.iface_manager()).await?;
            // Expose the same InterfaceManager through Reticulum<Running>.
            data.interface_manager = transport.iface_manager();
            data.transport = Some(transport);
        } else {
            log::debug!("Transport layer disabled");
            initialize_interfaces(&data.config, &data.interface_manager).await?;
        }

        log::info!("Reticulum started successfully");

        Ok(Reticulum {
            data,
            _state: PhantomData,
        })
    }
}

// ── Running: live-node accessors ──────────────────────────────────────────────

impl Reticulum<Running> {
    /// The node's private identity.
    ///
    /// Only accessible after the node has started.  The identity lives
    /// exclusively here — no `Arc`, no clone ever leaves this struct.
    pub fn identity(&self) -> &PrivateIdentity {
        &self.data.identity
    }

    /// The transport layer, if enabled in config.
    pub fn transport(&self) -> Option<&Arc<Transport>> {
        self.data.transport.as_ref()
    }

    /// The interface manager.
    pub fn interface_manager(&self) -> &Arc<Mutex<InterfaceManager>> {
        &self.data.interface_manager
    }
}

// ── Interface initialisation ──────────────────────────────────────────────────

async fn initialize_interfaces(
    config: &Config,
    interface_manager: &Arc<Mutex<InterfaceManager>>,
) -> Result<(), ReticulumError> {
    log::debug!("Initializing interfaces");
    let mut iface_mgr = interface_manager.lock().await;

    for (name, iface_config) in &config.interfaces {
        match iface_config {
            InterfaceConfig::Tcp(tcp_config) => {
                if !tcp_config.enabled {
                    continue;
                }
                let addr = format!("{}:{}", tcp_config.address, tcp_config.port);
                log::info!("Initializing TCP interface '{}' ({})", name, addr);
                match tcp_config.mode.as_str() {
                    "server" => {
                        let server = TcpServer::new(addr, interface_manager.clone());
                        iface_mgr.spawn(server, TcpServer::spawn);
                    }
                    "client" => {
                        iface_mgr.spawn(TcpClient::new(addr), TcpClient::spawn);
                    }
                    mode => {
                        log::warn!("Unknown TCP mode '{}' for interface '{}'", mode, name);
                    }
                }
            }
            InterfaceConfig::Udp(udp_config) => {
                if !udp_config.enabled {
                    continue;
                }
                let bind_addr = format!("{}:{}", udp_config.address, udp_config.port);
                let forward_addr = udp_config.forward_broadcasts.then(|| bind_addr.clone());
                iface_mgr.spawn(
                    UdpInterface::new(bind_addr, forward_addr),
                    UdpInterface::spawn,
                );
            }
            InterfaceConfig::Auto(auto_config) => {
                if !auto_config.enabled {
                    continue;
                }
                iface_mgr.spawn(
                    AutoInterface::new(
                        auto_config.group.clone(),
                        auto_config.discovery_port,
                        auto_config.data_port,
                    ),
                    AutoInterface::spawn,
                );
            }
            InterfaceConfig::Serial(serial_config) => {
                if !serial_config.enabled {
                    continue;
                }
                iface_mgr.spawn(
                    SerialInterface::new(
                        serial_config.port.clone(),
                        serial_config.baud_rate,
                        serial_config.data_bits,
                        serial_config.parity.clone(),
                        serial_config.stop_bits,
                    ),
                    SerialInterface::spawn,
                );
            }
            InterfaceConfig::I2P(i2p_config) => {
                if !i2p_config.enabled {
                    continue;
                }
                iface_mgr.spawn(
                    I2PInterface::new(
                        i2p_config.sam_host.clone(),
                        i2p_config.sam_port,
                        i2p_config.destination.clone(),
                    ),
                    I2PInterface::spawn,
                );
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_paths() {
        let paths = ReticulumPaths::from_config_dir("/tmp/test_reticulum");
        assert_eq!(
            paths.config_path,
            PathBuf::from("/tmp/test_reticulum/config.toml")
        );
        assert_eq!(
            paths.storage_path,
            PathBuf::from("/tmp/test_reticulum/storage")
        );
    }

    #[test]
    fn test_constants() {
        assert_eq!(constants::MTU, 500);
        assert_eq!(
            constants::MDU,
            constants::MTU - constants::HEADER_MAXSIZE - constants::IFAC_MIN_SIZE
        );
    }

    #[tokio::test]
    async fn test_create_reticulum() {
        let temp_dir = std::env::temp_dir().join("reticulum_test");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let node = Reticulum::new(Some(temp_dir.clone()))
            .await
            .expect("new node");
        assert!(node.paths().config_path.exists());

        // Compile-time proof: the lines below would not compile if uncommented,
        // because identity() and transport() don't exist on Reticulum<Unstarted>:
        //   node.identity();
        //   node.transport();

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_start_reticulum() {
        let temp_dir = std::env::temp_dir().join("reticulum_start_test");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let node = Reticulum::new(Some(temp_dir.clone()))
            .await
            .expect("new node");

        let running = node.start().await.expect("start node");

        // identity() is only accessible here, after start()
        let _addr = running.identity().address_hash();

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_identity_persistence() {
        let dir = std::env::temp_dir().join("reticulum_identity_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity");
        let _ = std::fs::remove_file(&path);

        let id1 = load_or_create_identity(&path).expect("create identity");
        assert!(path.exists());

        let id2 = load_or_create_identity(&path).expect("load identity");

        assert_eq!(
            id1.address_hash(),
            id2.address_hash(),
            "address hash must be stable across restarts"
        );
        assert_eq!(
            id1.to_hex_string(),
            id2.to_hex_string(),
            "private key bytes must round-trip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
