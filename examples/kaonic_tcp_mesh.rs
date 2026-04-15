use std::env;
use std::sync::Arc;

use reticulum::iface::kaonic::kaonic_grpc::KaonicGrpc;
use reticulum::iface::kaonic::{RadioConfig, RadioModule};
use reticulum::iface::tcp_client::TcpClient;
use reticulum::transport::{Transport, TransportConfig};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    let args: Vec<String> = env::args().collect();

    let mut config = TransportConfig::default();
    config.set_retransmit(true);
    config.set_broadcast(false);

    let transport = Arc::new(Mutex::new(Transport::new(config)));

    if args.len() < 3 {
        println!("Usage: {} <tcp-server> <kaonic-grpc>", args[0]);
        return;
    }

    log::info!("start kaonic client");

    let cancel = tokio_util::sync::CancellationToken::new();
    KaonicGrpc::new(
        format!("http://{}", &args[2]),
        RadioConfig::new_for_module(RadioModule::RadioA),
        None,
    )
    .register(&mut *transport.lock().await.iface_manager().lock().await, cancel);

    log::info!("start tcp client");

    let client = TcpClient::connect(&args[1]).await.expect("tcp connect");
    transport.lock().await.iface_manager().lock().await.spawn_interface(client);

    log::info!("start tcp client");

    let _ = tokio::signal::ctrl_c().await;
}
