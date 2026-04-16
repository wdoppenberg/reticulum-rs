use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Errors that can occur during configuration loading
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Validation(String),
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "IO error: {}", e),
            ConfigError::Parse(e) => write!(f, "Parse error: {}", e),
            ConfigError::Validation(s) => write!(f, "Validation error: {}", s),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Main Reticulum configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub reticulum: ReticulumConfig,

    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(default)]
    pub interfaces: HashMap<String, InterfaceConfig>,
}

/// Core Reticulum settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReticulumConfig {
    /// Whether to share this instance with other local processes
    #[serde(default = "default_true")]
    pub share_instance: bool,

    /// Name for the local instance socket (Unix only)
    #[serde(default)]
    pub instance_name: Option<String>,

    /// Port for the shared instance interface
    #[serde(default = "default_interface_port")]
    pub shared_instance_port: u16,

    /// Port for instance control/RPC
    #[serde(default = "default_control_port")]
    pub instance_control_port: u16,

    /// RPC authentication key (hex string)
    #[serde(default)]
    pub rpc_key: Option<String>,

    /// Enable transport/routing functionality
    #[serde(default)]
    pub enable_transport: bool,

    /// Path to network identity file
    #[serde(default)]
    pub network_identity: Option<PathBuf>,

    /// Enable automatic link MTU discovery
    #[serde(default = "default_true")]
    pub link_mtu_discovery: bool,

    /// Enable remote management
    #[serde(default)]
    pub enable_remote_management: bool,

    /// Use implicit proofs
    #[serde(default = "default_true")]
    pub use_implicit_proof: bool,

    /// Allow network probes
    #[serde(default)]
    pub allow_probes: bool,

    /// Enable discovery features
    #[serde(default)]
    pub enable_discovery: bool,

    /// Auto-discover network interfaces
    #[serde(default)]
    pub discover_interfaces: bool,

    /// Auto-connect to discovered interfaces
    #[serde(default)]
    pub autoconnect_discovered_interfaces: bool,

    /// Panic on interface errors
    #[serde(default)]
    pub panic_on_interface_error: bool,
}

impl Default for ReticulumConfig {
    fn default() -> Self {
        Self {
            share_instance: true,
            instance_name: None,
            shared_instance_port: 37428,
            instance_control_port: 37429,
            rpc_key: None,
            // Transport is enabled by default — applications that need to link,
            // announce, and route (e.g. chat) cannot function without it.
            enable_transport: true,
            network_identity: None,
            link_mtu_discovery: true,
            enable_remote_management: false,
            use_implicit_proof: true,
            allow_probes: false,
            enable_discovery: false,
            discover_interfaces: false,
            autoconnect_discovered_interfaces: false,
            panic_on_interface_error: false,
        }
    }
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (0-7, where 7 is most verbose)
    #[serde(default = "default_loglevel")]
    pub loglevel: u8,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { loglevel: 4 }
    }
}

/// Interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InterfaceConfig {
    Tcp(TcpInterfaceConfig),
    Udp(UdpInterfaceConfig),
    Auto(AutoInterfaceConfig),
    Serial(SerialInterfaceConfig),
    I2P(I2PInterfaceConfig),
}

/// TCP interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInterfaceConfig {
    /// Enable this interface
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Interface mode: "server" or "client"
    pub mode: String,

    /// Listen address (server) or target address (client)
    pub address: String,

    /// Port number
    pub port: u16,

    /// Interface bitrate in bits per second
    #[serde(default)]
    pub bitrate: Option<u64>,

    /// Outbound interface
    #[serde(default = "default_true")]
    pub outbound: bool,
}

/// UDP interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpInterfaceConfig {
    /// Enable this interface
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Listen address
    pub address: String,

    /// Port number
    pub port: u16,

    /// Forward broadcasts
    #[serde(default = "default_true")]
    pub forward_broadcasts: bool,

    /// Interface bitrate in bits per second
    #[serde(default)]
    pub bitrate: Option<u64>,

    /// Outbound interface
    #[serde(default = "default_true")]
    pub outbound: bool,
}

/// Auto interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoInterfaceConfig {
    /// Enable this interface
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Multicast group
    #[serde(default)]
    pub group: Option<String>,

    /// Discovery port
    #[serde(default)]
    pub discovery_port: Option<u16>,

    /// Data port
    #[serde(default)]
    pub data_port: Option<u16>,
}

/// Serial interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialInterfaceConfig {
    /// Enable this interface
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Serial port path
    pub port: String,

    /// Baud rate
    pub baud_rate: u32,

    /// Data bits
    #[serde(default = "default_databits")]
    pub data_bits: u8,

    /// Parity (None, Odd, Even)
    #[serde(default)]
    pub parity: Option<String>,

    /// Stop bits
    #[serde(default = "default_stopbits")]
    pub stop_bits: u8,
}

/// I2P interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I2PInterfaceConfig {
    /// Enable this interface
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// SAM API host
    #[serde(default = "default_i2p_host")]
    pub sam_host: String,

    /// SAM API port
    #[serde(default = "default_i2p_port")]
    pub sam_port: u16,

    /// I2P destination (leave empty to generate)
    #[serde(default)]
    pub destination: Option<String>,
}

// Default value functions
fn default_true() -> bool {
    true
}

fn default_interface_port() -> u16 {
    37428
}

fn default_control_port() -> u16 {
    37429
}

fn default_loglevel() -> u8 {
    4
}

fn default_databits() -> u8 {
    8
}

fn default_stopbits() -> u8 {
    1
}

fn default_i2p_host() -> String {
    "127.0.0.1".to_string()
}

fn default_i2p_port() -> u16 {
    7656
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Create a default configuration.
    ///
    /// Transport is enabled and a single `AutoInterface` (local multicast
    /// discovery) is included so a fresh node can communicate on the local
    /// network without any manual configuration.
    pub fn default_config() -> Self {
        let mut interfaces = HashMap::new();
        interfaces.insert(
            "auto".to_string(),
            InterfaceConfig::Auto(AutoInterfaceConfig {
                enabled: true,
                group: None,
                discovery_port: None,
                data_port: None,
            }),
        );
        Self {
            reticulum: ReticulumConfig::default(),
            logging: LoggingConfig::default(),
            interfaces,
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate log level
        if self.logging.loglevel > 7 {
            return Err(ConfigError::Validation(
                "Log level must be between 0 and 7".to_string(),
            ));
        }

        // Validate interface configurations
        for (name, iface) in &self.interfaces {
            match iface {
                InterfaceConfig::Tcp(tcp) => {
                    if tcp.mode != "server" && tcp.mode != "client" {
                        return Err(ConfigError::Validation(format!(
                            "TCP interface '{}': mode must be 'server' or 'client'",
                            name
                        )));
                    }
                }
                InterfaceConfig::Serial(serial) => {
                    if serial.data_bits < 5 || serial.data_bits > 8 {
                        return Err(ConfigError::Validation(format!(
                            "Serial interface '{}': data_bits must be 5-8",
                            name
                        )));
                    }
                    if serial.stop_bits < 1 || serial.stop_bits > 2 {
                        return Err(ConfigError::Validation(format!(
                            "Serial interface '{}': stop_bits must be 1 or 2",
                            name
                        )));
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Generate a default configuration file content.
    ///
    /// Transport is enabled and an `AutoInterface` is active so the node works
    /// on a local network without any further editing.
    pub fn generate_default_toml() -> String {
        r#"# Reticulum Network Stack Configuration
# Generated automatically on first run.  Edit and restart to apply changes.

[reticulum]
# Enable routing/transport functionality.  Must be true for chat applications.
enable_transport = true

# Share this instance with other local processes
share_instance = true

# Port for shared instance communication
shared_instance_port = 37428

# Port for instance control/RPC
instance_control_port = 37429

# Enable automatic link MTU discovery
link_mtu_discovery = true

# Use implicit proofs
use_implicit_proof = true

# Allow network probes
allow_probes = false

# Panic on interface errors
panic_on_interface_error = false

[logging]
# Log level (0-7 where 7 is most verbose)
# 0 = Critical  1 = Error  2 = Warning  3 = Notice
# 4 = Info      5 = Verbose 6 = Debug   7 = Extreme
loglevel = 4

# ── Interfaces ────────────────────────────────────────────────────────────────
# AutoInterface: discovers and connects to nearby Reticulum nodes over the
# local network using multicast UDP.  Works on most LANs with no extra setup.
[interfaces.auto]
type   = "auto"
enabled = true

# TCP server — accept connections from other nodes:
# [interfaces.tcp_server]
# type    = "tcp"
# enabled = true
# mode    = "server"
# address = "0.0.0.0"
# port    = 4242

# TCP client — connect to a known Reticulum node:
# [interfaces.tcp_client]
# type    = "tcp"
# enabled = true
# mode    = "client"
# address = "192.168.1.2"
# port    = 4242
"#
        .to_string()
    }

    /// Save configuration to a TOML file
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), ConfigError> {
        let contents =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Validation(e.to_string()))?;
        fs::write(path, contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default_config();
        assert!(config.reticulum.share_instance);
        assert_eq!(config.reticulum.shared_instance_port, 37428);
        assert_eq!(config.logging.loglevel, 4);
    }

    #[test]
    fn test_config_validation() {
        let config = Config::default_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_loglevel() {
        let mut config = Config::default_config();
        config.logging.loglevel = 10;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_toml() {
        let toml_str = r#"
[reticulum]
share_instance = false
enable_transport = true

[logging]
loglevel = 5

[interfaces.tcp_test]
type = "tcp"
enabled = true
mode = "server"
address = "127.0.0.1"
port = 4242
outbound = true
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.reticulum.share_instance);
        assert!(config.reticulum.enable_transport);
        assert_eq!(config.logging.loglevel, 5);
        assert_eq!(config.interfaces.len(), 1);

        if let Some(InterfaceConfig::Tcp(tcp)) = config.interfaces.get("tcp_test") {
            assert!(tcp.enabled);
            assert_eq!(tcp.mode, "server");
            assert_eq!(tcp.port, 4242);
        } else {
            panic!("Expected TCP interface");
        }
    }
}
