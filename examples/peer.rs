/// Two-peer connectivity demo.
///
/// Run in two separate terminals:
///
///   Terminal 1 (server):  cargo run --example peer -- server
///   Terminal 2 (client):  cargo run --example peer -- client
///
/// Each peer announces itself every 3 seconds and prints any announce it
/// receives from the other side.

use std::sync::Arc;
use std::time::Duration;

use reticulum_core::destination::DestinationName;
use reticulum_core::identity::PrivateIdentity;
use reticulum_tokio::tcp_client::TcpClient;
use reticulum_tokio::tcp_server::TcpServer;
use reticulum_tokio::{Transport, TransportConfig};

const SERVER_ADDR: &str = "127.0.0.1:9280";

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();

    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "server".to_string());

    if mode != "server" && mode != "client" {
        eprintln!("Usage: peer [server|client]");
        std::process::exit(1);
    }

    // Deterministic identity derived from the role name so the address is
    // stable across restarts.  In production, load from disk instead.
    let identity = PrivateIdentity::new_from_name(&mode);
    let node_address = *identity.address_hash();

    let mut transport = Transport::new(TransportConfig::new(&mode, node_address, true));

    match mode.as_str() {
        "server" => {
            transport.iface_manager().lock().await.spawn(
                TcpServer::new(SERVER_ADDR, transport.iface_manager()),
                TcpServer::spawn,
            );
            println!("[{mode}] Listening on {SERVER_ADDR}");
        }
        _ => {
            transport
                .iface_manager()
                .lock()
                .await
                .spawn(TcpClient::new(SERVER_ADDR), TcpClient::spawn);
            println!("[{mode}] Connecting to {SERVER_ADDR}");
        }
    }

    // Register this peer's destination (consumes identity — must happen before
    // the transport is wrapped in Arc).
    let dest = transport
        .add_destination(identity, DestinationName::new("example", "peer"))
        .await;

    println!("[{mode}] My address: {}", dest.lock().await.desc.address_hash);

    // Subscribe to incoming announces before starting to send, so none are
    // missed if the connection is already live.
    let mut incoming = transport.recv_announces().await;

    let transport = Arc::new(transport);

    // Periodic announce task.
    {
        let transport = transport.clone();
        let mode = mode.clone();
        tokio::spawn(async move {
            // Give the TCP layer a moment to establish the connection.
            tokio::time::sleep(Duration::from_millis(500)).await;
            loop {
                transport.send_announce(&dest, None).await;
                println!("[{mode}] → sent announce");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    // Receive loop — print every announce that arrives from the other peer.
    println!("[{mode}] Waiting for peer announces (Ctrl+C to stop)...");
    loop {
        match incoming.recv().await {
            Ok(event) => {
                let addr = event.destination.lock().await.desc.address_hash;
                println!("[{mode}] ← received announce from {addr}");
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                println!("[{mode}] ! dropped {n} announces (receiver too slow)");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                println!("[{mode}] channel closed, exiting");
                break;
            }
        }
    }
}
