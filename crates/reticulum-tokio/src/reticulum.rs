use crate::config::{Config, ConfigError, InterfaceConfig};
use crate::iface::InterfaceManager;
use crate::transport::{Transport, TransportConfig};
use reticulum_core::identity::PrivateIdentity;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Errors that can occur during Reticulum initialization
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
            ReticulumError::AlreadyInitialized => write!(f, "Reticulum instance already initialized"),
        }
    }
}

impl std::error::Error for ReticulumError {}

/// Network constants
pub mod constants {
    use std::time::Duration;

    /// Maximum Transmission Unit (MTU) for Reticulum packets
    /// Future minimum will probably be locked in at 251 bytes to support
    /// networks with segments of different MTUs. Absolute minimum is 219.
    pub const MTU: usize = 500;

    /// Whether automatic link MTU discovery is enabled by default
    pub const LINK_MTU_DISCOVERY: bool = true;

    /// Maximum percentage of interface bandwidth for announces
    pub const ANNOUNCE_CAP: u8 = 2;

    /// Minimum bitrate required for Reticulum (bits per second)
    pub const MINIMUM_BITRATE: u64 = 5;

    /// Default per-hop timeout in seconds
    pub const DEFAULT_PER_HOP_TIMEOUT: u64 = 6;

    /// Length of truncated hashes in bits
    pub const TRUNCATED_HASHLENGTH: usize = 128;

    /// Minimum header size in bytes
    pub const HEADER_MINSIZE: usize = 2 + 1 + (TRUNCATED_HASHLENGTH / 8) * 1;

    /// Maximum header size in bytes
    pub const HEADER_MAXSIZE: usize = 2 + 1 + (TRUNCATED_HASHLENGTH / 8) * 2;

    /// Minimum IFAC size
    pub const IFAC_MIN_SIZE: usize = 1;

    /// Maximum Data Unit (payload size after headers)
    pub const MDU: usize = MTU - HEADER_MAXSIZE - IFAC_MIN_SIZE;

    /// Resource cache duration
    pub const RESOURCE_CACHE: Duration = Duration::from_secs(24 * 60 * 60);

    /// Job interval
    pub const JOB_INTERVAL: Duration = Duration::from_secs(5 * 60);

    /// Cache cleanup interval
    pub const CLEAN_INTERVAL: Duration = Duration::from_secs(15 * 60);

    /// Data persistence interval
    pub const PERSIST_INTERVAL: Duration = Duration::from_secs(60 * 60 * 12);

    /// Gracious persist interval
    pub const GRACIOUS_PERSIST_INTERVAL: Duration = Duration::from_secs(60 * 5);

    /// Maximum queued announces
    pub const MAX_QUEUED_ANNOUNCES: usize = 16384;

    /// Queued announce lifetime
    pub const QUEUED_ANNOUNCE_LIFE: Duration = Duration::from_secs(60 * 60 * 24);
}

/// Configuration directories and paths
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
    /// Create paths from a config directory
    pub fn from_config_dir<P: AsRef<Path>>(config_dir: P) -> Self {
        let config_dir = config_dir.as_ref().to_path_buf();
        Self {
            config_path: config_dir.join("config"),
            storage_path: config_dir.join("storage"),
            cache_path: config_dir.join("storage").join("cache"),
            resource_path: config_dir.join("storage").join("resources"),
            identity_path: config_dir.join("storage").join("identities"),
            interface_path: config_dir.join("interfaces"),
            config_dir,
        }
    }

    /// Determine default config directory
    pub fn default_config_dir() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            let etc_path = PathBuf::from("/etc/reticulum");
            let xdg_path = PathBuf::from(&home).join(".config").join("reticulum");
            let home_path = PathBuf::from(&home).join(".reticulum");

            // Check /etc/reticulum first
            if etc_path.join("config").exists() {
                return etc_path;
            }

            // Check ~/.config/reticulum
            if xdg_path.join("config").exists() {
                return xdg_path;
            }

            // Default to ~/.reticulum
            home_path
        } else {
            PathBuf::from(".reticulum")
        }
    }

    /// Create all necessary directories
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

/// Main Reticulum instance
///
/// This class is used to initialise access to Reticulum within a program.
/// You must create exactly one instance of this class before carrying out
/// any other RNS operations, such as creating destinations or sending traffic.
///
/// As soon as an instance of this class is created, Reticulum will start
/// opening and configuring any hardware devices specified in the supplied
/// configuration.
pub struct Reticulum {
    config: Config,
    paths: ReticulumPaths,
    identity: PrivateIdentity,
    transport: Option<Arc<Transport>>,
    interface_manager: Arc<Mutex<InterfaceManager>>,
    cancellation_token: CancellationToken,
}

impl Reticulum {
    /// Initialize Reticulum with a specific config directory
    pub async fn new(config_dir: Option<PathBuf>) -> Result<Self, ReticulumError> {
        let paths = if let Some(dir) = config_dir {
            ReticulumPaths::from_config_dir(dir)
        } else {
            ReticulumPaths::from_config_dir(ReticulumPaths::default_config_dir())
        };

        Self::new_with_paths(paths).await
    }

    /// Initialize Reticulum with specific paths
    pub async fn new_with_paths(paths: ReticulumPaths) -> Result<Self, ReticulumError> {
        // Create necessary directories
        paths.create_directories()?;

        // Load or create configuration
        let config = if paths.config_path.exists() {
            log::info!(
                "Loading configuration from {}",
                paths.config_path.display()
            );
            Config::from_file(&paths.config_path)?
        } else {
            log::info!("Creating default configuration file");
            let config = Config::default_config();
            std::fs::write(&paths.config_path, Config::generate_default_toml())?;
            log::info!(
                "Default config file created at {}. Make any necessary changes and restart if needed.",
                paths.config_path.display()
            );
            config
        };

        // Load or create network identity
        let identity = if let Some(identity_path) = &config.reticulum.network_identity {
            let full_path = if identity_path.is_absolute() {
                identity_path.clone()
            } else {
                paths.config_dir.join(identity_path)
            };

            if full_path.exists() {
                log::debug!("Loading network identity from {}", full_path.display());
                // TODO: Implement identity loading from file
                PrivateIdentity::new_from_rand(rand_core::OsRng)
            } else {
                log::info!("Generating new network identity");
                let identity = PrivateIdentity::new_from_rand(rand_core::OsRng);
                // TODO: Implement identity saving to file
                log::info!("Network identity saved to {}", full_path.display());
                identity
            }
        } else {
            // Generate ephemeral identity
            log::debug!("Using ephemeral network identity");
            PrivateIdentity::new_from_rand(rand_core::OsRng)
        };

        // Initialize interface manager with rx channel capacity
        let interface_manager = Arc::new(Mutex::new(InterfaceManager::new(256)));

        let cancellation_token = CancellationToken::new();

        Ok(Self {
            config,
            paths,
            identity,
            transport: None,
            interface_manager,
            cancellation_token,
        })
    }

    /// Start the Reticulum instance
    pub async fn start(&mut self) -> Result<(), ReticulumError> {
        log::info!("Starting Reticulum Network Stack");

        // Initialize interfaces from config
        self.initialize_interfaces().await?;

        // Start transport if enabled
        if self.config.reticulum.enable_transport {
            log::info!("Starting transport layer");
            let transport_config = TransportConfig::new("main", &self.identity, true);

            let transport = Transport::new(transport_config);

            self.transport = Some(Arc::new(transport));
        } else {
            log::debug!("Transport layer disabled");
        }

        log::info!("Reticulum started successfully");
        Ok(())
    }

    /// Initialize interfaces from configuration
    async fn initialize_interfaces(&mut self) -> Result<(), ReticulumError> {
        log::debug!("Initializing interfaces");

        let _iface_mgr = self.interface_manager.lock().await;

        for (name, iface_config) in &self.config.interfaces {
            match iface_config {
                InterfaceConfig::Tcp(tcp_config) => {
                    if !tcp_config.enabled {
                        log::debug!("Skipping disabled TCP interface '{}'", name);
                        continue;
                    }
                    log::info!("Initializing TCP interface '{}' ({}:{})", name, tcp_config.address, tcp_config.port);
                    // TODO: Initialize TCP interface
                }
                InterfaceConfig::Udp(udp_config) => {
                    if !udp_config.enabled {
                        log::debug!("Skipping disabled UDP interface '{}'", name);
                        continue;
                    }
                    log::info!("Initializing UDP interface '{}' ({}:{})", name, udp_config.address, udp_config.port);
                    // TODO: Initialize UDP interface
                }
                InterfaceConfig::Auto(auto_config) => {
                    if !auto_config.enabled {
                        log::debug!("Skipping disabled Auto interface '{}'", name);
                        continue;
                    }
                    log::info!("Initializing Auto interface '{}'", name);
                    // TODO: Initialize Auto interface
                }
                InterfaceConfig::Serial(serial_config) => {
                    if !serial_config.enabled {
                        log::debug!("Skipping disabled Serial interface '{}'", name);
                        continue;
                    }
                    log::info!("Initializing Serial interface '{}' ({})", name, serial_config.port);
                    // TODO: Initialize Serial interface
                }
                InterfaceConfig::I2P(i2p_config) => {
                    if !i2p_config.enabled {
                        log::debug!("Skipping disabled I2P interface '{}'", name);
                        continue;
                    }
                    log::info!("Initializing I2P interface '{}'", name);
                    // TODO: Initialize I2P interface
                }
            }
        }

        Ok(())
    }

    /// Get the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get the paths
    pub fn paths(&self) -> &ReticulumPaths {
        &self.paths
    }

    /// Get the identity
    pub fn identity(&self) -> &PrivateIdentity {
        &self.identity
    }

    /// Get the transport (if enabled)
    pub fn transport(&self) -> Option<&Arc<Transport>> {
        self.transport.as_ref()
    }

    /// Get the interface manager
    pub fn interface_manager(&self) -> &Arc<Mutex<InterfaceManager>> {
        &self.interface_manager
    }

    /// Shutdown the Reticulum instance
    pub async fn shutdown(&self) {
        log::info!("Shutting down Reticulum");
        self.cancellation_token.cancel();

        // TODO: Persist data, clean up resources, etc.

        log::info!("Reticulum shutdown complete");
    }
}

impl Drop for Reticulum {
    fn drop(&mut self) {
        log::debug!("Reticulum instance dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_paths() {
        let paths = ReticulumPaths::from_config_dir("/tmp/test_reticulum");
        assert_eq!(
            paths.config_path,
            PathBuf::from("/tmp/test_reticulum/config")
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

        let result = Reticulum::new(Some(temp_dir.clone())).await;
        assert!(result.is_ok());

        let reticulum = result.unwrap();
        assert!(reticulum.paths.config_path.exists());

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
