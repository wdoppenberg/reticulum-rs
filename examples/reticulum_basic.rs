/// Basic example demonstrating Reticulum stack initialization
///
/// This example shows how to:
/// - Initialize Reticulum with a configuration file
/// - Start the network stack
/// - Access the configuration and identity
///
/// Usage:
///   cargo run --example reticulum_basic
///
/// To use a custom config:
///   cargo run --example reticulum_basic -- /path/to/config/dir

use reticulum_tokio::{Reticulum, ReticulumError};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), ReticulumError> {
    // Initialize logging
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    log::info!("Reticulum Basic Example");

    // Get config directory from command line args or use default
    let config_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from);

    // Initialize Reticulum instance
    let mut reticulum = Reticulum::new(config_dir).await?;

    // Display configuration information
    log::info!("Configuration loaded from: {}", reticulum.paths().config_path.display());
    log::info!("Storage path: {}", reticulum.paths().storage_path.display());
    log::info!("Transport enabled: {}", reticulum.config().reticulum.enable_transport);
    log::info!("Share instance: {}", reticulum.config().reticulum.share_instance);
    log::info!("Log level: {}", reticulum.config().logging.loglevel);

    // Display identity information
    let identity = reticulum.identity();
    log::info!("Network identity hash: {}", identity.hash());

    // Display interface configuration
    log::info!("Configured interfaces: {}", reticulum.config().interfaces.len());
    for (name, iface_config) in &reticulum.config().interfaces {
        log::info!("  - {} ({:?})", name, match iface_config {
            reticulum_tokio::InterfaceConfig::Tcp(_) => "TCP",
            reticulum_tokio::InterfaceConfig::Udp(_) => "UDP",
            reticulum_tokio::InterfaceConfig::Auto(_) => "Auto",
            reticulum_tokio::InterfaceConfig::Serial(_) => "Serial",
            reticulum_tokio::InterfaceConfig::I2P(_) => "I2P",
        });
    }

    // Start the Reticulum stack
    log::info!("Starting Reticulum...");
    reticulum.start().await?;

    log::info!("Reticulum is running!");
    log::info!("Press Ctrl+C to shutdown...");

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");

    log::info!("Shutdown signal received");
    reticulum.shutdown().await;

    log::info!("Reticulum stopped");

    Ok(())
}
