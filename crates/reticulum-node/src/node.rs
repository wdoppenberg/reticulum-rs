//! Embassy-based async node runner.
//!
//! [`run`] is the top-level async function that wires together an
//! [`InterfaceRouter`], a [`Router`], and an RX channel into a full
//! Reticulum forwarding node.
//!
//! # Structure
//!
//! ```text
//!  ┌─────────────┐  RxMessage  ┌─────────────────────────────────┐
//!  │  Interface  │────────────▶│                                 │
//!  │  driver     │             │  run()  loop                    │
//!  │  tasks      │◀────────────│    Router::route()              │
//!  └─────────────┘  TxMessage  │    Router::increment_hops()     │
//!                               │    InterfaceRouter::route()     │
//!                               └─────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
//! use reticulum_embassy::iface::{RxMessage, TxMessage, InterfaceRouter};
//! use reticulum_node::node::run;
//! use reticulum_node::config::NodeConfig;
//!
//! static RX: Channel<CriticalSectionRawMutex, RxMessage, 4> = Channel::new();
//! static TX0: Channel<CriticalSectionRawMutex, TxMessage, 4> = Channel::new();
//!
//! // In your embassy task:
//! let config = NodeConfig::embedded();
//! let mut router = InterfaceRouter::<4>::new();
//! router.register(iface_addr, TX0.sender().into()).unwrap();
//!
//! run::<64, 32, 4>(config, node_addr, router, RX.receiver().into()).await;
//! ```

use embassy_sync::channel::DynamicReceiver;

use reticulum_core::hash::AddressHash;
use reticulum_core::routing::{RxMessage, TxMessage, TxMessageType};

use reticulum_embassy::iface::InterfaceRouter;

use crate::config::NodeConfig;
use crate::router::{RouteDecision, Router};

// ── run() ─────────────────────────────────────────────────────────────────────

/// Run the Reticulum forwarding loop.
///
/// - `N_SEEN` — dedup ring-buffer capacity (power-of-two recommended).
/// - `N_PATHS` — path-table capacity.
/// - `N_IFACES` — maximum interfaces registered in the [`InterfaceRouter`].
///
/// `node_addr` is the local node's [`AddressHash`], derived from its
/// [`PrivateIdentity`](reticulum_core::identity::PrivateIdentity).  It is used
/// to detect packets addressed to this node (transport relay matching and local
/// delivery) without exposing the private key to the forwarding loop.
///
/// This function never returns under normal operation.
pub async fn run<const N_SEEN: usize, const N_PATHS: usize, const N_IFACES: usize>(
    config: NodeConfig,
    node_addr: AddressHash,
    iface_router: InterfaceRouter<'_, N_IFACES>,
    rx: DynamicReceiver<'_, RxMessage>,
) {
    let mut router = Router::<N_SEEN, N_PATHS>::new(
        node_addr,
        config.forward_announces,
        config.transport_enabled,
    );

    loop {
        let msg = rx.receive().await;
        let decision = router.route(&msg.packet, msg.address);

        match decision {
            RouteDecision::Broadcast => {
                let mut pkt = msg.packet;
                Router::<N_SEEN, N_PATHS>::increment_hops(&mut pkt);
                let tx = TxMessage {
                    tx_type: TxMessageType::Broadcast(Some(msg.address)),
                    packet: pkt,
                };
                iface_router.route(tx).await;
            }
            RouteDecision::Direct(target_iface) => {
                let mut pkt = msg.packet;
                Router::<N_SEEN, N_PATHS>::increment_hops(&mut pkt);
                let tx = TxMessage {
                    tx_type: TxMessageType::Direct(target_iface),
                    packet: pkt,
                };
                iface_router.route(tx).await;
            }
            RouteDecision::Local => {
                // Packet addressed to this node.  A full node would dispatch it
                // to a local destination handler; for a pure forwarding node
                // this is a no-op.  Log and discard.
                log::debug!("node: received local packet (destination=self)");
            }
            RouteDecision::Drop => {
                // Deduplicated, hop-limit exceeded, or transport-forwarding
                // mismatch — discard silently.
            }
        }
    }
}
