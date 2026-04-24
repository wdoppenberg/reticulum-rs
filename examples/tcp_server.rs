use rand_core::OsRng;
use reticulum::identity::PrivateIdentity;
use reticulum::iface::tcp_server::TcpServer;
use reticulum::transport::{Transport, TransportConfig};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    log::info!(">>> TCP SERVER APP <<<");

    let transport = Transport::new(TransportConfig::new(
        "server",
        &PrivateIdentity::try_new_from_rand(OsRng).expect("os rng"),
        true,
    ));

    let mgr = transport.iface_manager().clone();
    let cancel = tokio_util::sync::CancellationToken::new();
    tokio::spawn(async move { TcpServer::new("0.0.0.0:4242").run(mgr, cancel).await });

    let _ = tokio::signal::ctrl_c().await;

    log::info!("exit");
}
