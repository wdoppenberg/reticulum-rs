use rand_core::OsRng;
use reticulum_core::identity::PrivateIdentity;
use reticulum_core::packet::Packet;
use reticulum_tokio::tcp_client::TcpClient;
use reticulum_tokio::tcp_server::TcpServer;
use reticulum_tokio::{Transport, TransportConfig};
use tokio_util::sync::CancellationToken;

async fn build_transport(name: &str, server_addr: &str, client_addr: &[&str]) -> Transport {
    let transport = Transport::new(TransportConfig::new(
        name,
        *PrivateIdentity::new_from_rand(OsRng).address_hash(),
        true,
    ));

    let cancel = CancellationToken::new();
    let mgr = transport.iface_manager().clone();
    let server_addr = server_addr.to_owned();
    tokio::spawn(async move { TcpServer::new(&server_addr).run(mgr, cancel).await });

    for &addr in client_addr {
        let client = {
            let mut retries = 0u32;
            loop {
                match TcpClient::connect(addr).await {
                    Ok(c) => break c,
                    Err(_) if retries < 20 => {
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => panic!("tcp connect to {addr} failed after retries: {e}"),
                }
            }
        };
        transport
            .iface_manager()
            .lock()
            .await
            .spawn_interface(client);
    }

    log::info!("test: transport {} created", name);

    transport
}

#[tokio::test]
async fn packet_overload() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace")).init();

    let transport_a = build_transport("a", "127.0.0.1:8081", &[]).await;
    let transport_b = build_transport("b", "127.0.0.1:8082", &["127.0.0.1:8081"]).await;

    let stop = CancellationToken::new();

    let producer_task = {
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut tx_counter = 0;

            let mut payload_size = 0;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => {
                            break;
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_micros(1)) => {

                        let mut packet = Packet::default();

                        packet.data.resize(payload_size);

                        payload_size += 1;
                        if payload_size >= 3072 {
                            payload_size = 0;
                        }

                        transport_a.send_packet(packet).await;
                        tx_counter += 1;
                    },
                };
            }

            tx_counter
        })
    };

    let consumer_task = {
        let stop = stop.clone();
        let mut messages = transport_b.iface_rx();
        tokio::spawn(async move {
            let mut rx_counter = 0;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => {
                            break;
                    },
                    Ok(_) = messages.recv() => {
                        rx_counter += 1;
                    },
                };
            }

            rx_counter
        })
    };

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    stop.cancel();

    let tx_counter = producer_task.await.unwrap();
    let rx_counter = consumer_task.await.unwrap();

    log::info!("TX: {}, RX: {}", tx_counter, rx_counter);
}
