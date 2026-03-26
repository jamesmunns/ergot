//! End-to-end tests: edge → bridge-router → root-router → edge.
//!
//! Topology:
//! ```text
//! Edge1 ←duplex→ Bridge ←duplex→ RootRouter ←duplex→ Edge2
//! ```
//! Edge1 is downstream of the Bridge. The Bridge's upstream connects
//! to the RootRouter. Edge2 is downstream of the RootRouter.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::time::Duration;

use common::{make_edge_stack, ping_with_retry, spawn_ping_server, wait_active};
use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::{direct_edge::EdgeFrameProcessor, router::Router},
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{sleep, timeout},
};

type RootStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type BridgeStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

/// Helper: register a COBS stream interface on a Router as downstream.
async fn register_router_downstream(
    stack: &RootStack,
    reader: impl AsyncRead + Unpin + Send + 'static,
    writer: impl AsyncWrite + Unpin + Send + 'static,
) -> u8 {
    tokio_cobs_stream::register_router(stack.clone(), reader, writer, 512, 4096, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn bridge_forwards_ping_upstream() {
    let _ = env_logger::builder().is_test(true).try_init();

    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let root_stack: RootStack = RootStack::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    // Duplex: Bridge upstream ↔ RootRouter downstream[0]
    let (bridge_up_read, root_d0_write) = tokio::io::duplex(8192);
    let (root_d0_read, bridge_up_write) = tokio::io::duplex(8192);

    // Duplex: Edge1 ↔ Bridge downstream[0]
    let (e1_read, bridge_d0_write) = tokio::io::duplex(8192);
    let (bridge_d0_read, e1_write) = tokio::io::duplex(8192);

    // Duplex: Edge2 ↔ RootRouter downstream[1]
    let (e2_read, root_d1_write) = tokio::io::duplex(8192);
    let (root_d1_read, e2_write) = tokio::io::duplex(8192);

    let _root_d0 = register_router_downstream(&root_stack, root_d0_read, root_d0_write).await;
    let _root_d1 = register_router_downstream(&root_stack, root_d1_read, root_d1_write).await;

    tokio_cobs_stream::register_bridge_upstream(
        bridge_stack.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_router(
        bridge_stack.clone(),
        bridge_d0_read,
        bridge_d0_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge1_stack.clone(),
        e1_read,
        e1_write,
        edge1_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Inactive,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge2_stack.clone(),
        e2_read,
        e2_write,
        edge2_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Inactive,
        None,
        None,
    )
    .await
    .unwrap();

    spawn_ping_server(&edge1_stack);
    spawn_ping_server(&edge2_stack);

    // Bootstrap edge2
    let edge2_via_root = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };
    ping_with_retry(&root_stack, edge2_via_root, 0).await;

    // Bootstrap bridge upstream
    let bridge_addr_from_root = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0,
    };
    let _ = timeout(
        Duration::from_millis(500),
        root_stack.endpoints().request::<ErgotPingEndpoint>(
            bridge_addr_from_root,
            &0u32,
            Some("ping"),
        ),
    )
    .await;

    sleep(Duration::from_millis(200)).await;

    // Bootstrap edge1 through bridge
    let bridge_d0_net = bridge_stack
        .manage_profile(|im| im.interface_state(0))
        .and_then(|s| match s {
            InterfaceState::Active { net_id, .. } => Some(net_id),
            _ => None,
        });

    if let Some(net_id) = bridge_d0_net {
        let edge1_via_bridge = Address {
            network_id: net_id,
            node_id: 2,
            port_id: 0,
        };
        let _ = timeout(
            Duration::from_millis(500),
            bridge_stack.endpoints().request::<ErgotPingEndpoint>(
                edge1_via_bridge,
                &0,
                Some("ping"),
            ),
        )
        .await;
    }

    wait_active(&edge2_stack).await;
    wait_active(&edge1_stack).await;

    // Root pings Edge2 directly
    let response = ping_with_retry(&root_stack, edge2_via_root, 42).await;
    assert_eq!(response, 42, "root → edge2 ping should work");

    // Bridge pings Edge1 (cross-bridge)
    if let Some(net_id) = bridge_d0_net {
        let edge1_via_bridge = Address {
            network_id: net_id,
            node_id: 2,
            port_id: 0,
        };
        let response = ping_with_retry(&bridge_stack, edge1_via_bridge, 77).await;
        assert_eq!(response, 77, "bridge → edge1 ping should work");
    }
}
