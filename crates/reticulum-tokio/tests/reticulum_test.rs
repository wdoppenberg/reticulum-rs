use std::sync::Once;
use std::time::Duration;

use reticulum_core::packet::Packet;
use reticulum_tokio::{Reticulum, ReticulumPaths};

static INIT: Once = Once::new();

fn setup() {
    INIT.call_once(|| {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init()
    });
}

/// Build a Reticulum node from an inline TOML config written to a temp dir.
async fn build_node(tag: &str, config_toml: &str) -> Reticulum<reticulum_tokio::Running> {
    let dir = std::env::temp_dir().join(format!("reticulum_two_node_test_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);

    let paths = ReticulumPaths::from_config_dir(&dir);
    paths.create_directories().expect("create dirs");
    std::fs::write(&paths.config_path, config_toml).expect("write config");

    let node = Reticulum::new_with_paths(paths).await.expect("new node");
    node.start().await.expect("start node")
}

#[tokio::test]
async fn two_nodes_connect_and_exchange_packets() {
    setup();

    // Node A: TCP server on 9181
    let config_a = r#"
[reticulum]
enable_transport = true

[interfaces.server]
type = "tcp"
enabled = true
mode = "server"
address = "127.0.0.1"
port = 9181
"#;

    // Node B: TCP client that connects to A
    let config_b = r#"
[reticulum]
enable_transport = true

[interfaces.client]
type = "tcp"
enabled = true
mode = "client"
address = "127.0.0.1"
port = 9181
"#;

    let node_a = build_node("a", config_a).await;
    let node_b = build_node("b", config_b).await;

    let transport_a = node_a.transport().expect("transport_a");
    let transport_b = node_b.transport().expect("transport_b");

    // Subscribe to raw interface frames on B before the packet is sent.
    let mut rx_b = transport_b.iface_rx();

    // Allow time for the TCP connection to establish.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Send a packet from A — the transport broadcasts it to all interfaces.
    transport_a.send_packet(Packet::default()).await;

    // B should receive the frame within a generous timeout.
    let result = tokio::time::timeout(Duration::from_secs(3), rx_b.recv()).await;

    assert!(
        result.is_ok(),
        "timed out waiting for packet on node B — TCP connection may not have been established"
    );
    assert!(
        result.unwrap().is_ok(),
        "broadcast channel closed unexpectedly"
    );

    log::info!("two_nodes_connect_and_exchange_packets: OK");
}
