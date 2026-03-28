//! End-to-end test for link-local addressing.
//!
//! Verifies that an edge device can initiate communication with net_id=0
//! (link-local) before receiving any frame from the router, and that it
//! correctly discovers its real net_id from the router's response.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::{pin::pin, time::Duration};

use common::make_edge_stack;
use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::EdgeFrameProcessor,
        profiles::router::Router,
        transports::tokio_cobs_stream,
    },
    net_stack::ArcNetStack,
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

type RouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

/// Spawn a ping server on the router stack.
fn spawn_router_ping_server(stack: &RouterStack) {
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<ErgotPingEndpoint, 4>(Some("ping"));
            let server = pin!(server);
            let mut hdl = server.attach();
            loop {
                let _ = hdl
                    .serve(|val: &u32| {
                        let v = *val;
                        async move { v }
                    })
                    .await;
            }
        }
    });
}

/// Edge initiates ping to the router using link-local addressing (net_id=0).
///
/// The edge has never received a frame from the router, so it doesn't know
/// its real net_id. It sends to (0, CENTRAL_NODE_ID, 0) which means
/// "my directly connected router, find the ping endpoint by key".
///
/// The router rewrites dst.network_id=0 to the real net_id, delivers
/// locally, and responds. The edge discovers its net_id from the response.
#[tokio::test]
async fn edge_initiates_link_local_ping() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: RouterStack = RouterStack::new();
    let (edge_stack, edge_queue) = make_edge_stack();

    // Create in-memory duplex pipes
    let (e_read, r_write) = tokio::io::duplex(8192);
    let (r_read, e_write) = tokio::io::duplex(8192);

    // Register router side
    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r_read,
        r_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // Register edge side — starts as Active { net_id: 0 }
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge_stack.clone(),
        e_read,
        e_write,
        edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: 2, // EDGE_NODE_ID
        },
        None,
        None,
    )
    .await
    .unwrap();

    // Start ping server on the ROUTER (not the edge)
    spawn_router_ping_server(&router_stack);

    // Edge pings router at link-local address: net_id=0, node_id=1 (CENTRAL_NODE_ID)
    let router_link_local = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0, // wildcard — find ping endpoint by key
    };

    // Edge initiates first! No router-to-edge ping happened before this.
    let response = timeout(
        Duration::from_secs(5),
        edge_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_link_local, &42, Some("ping")),
    )
    .await
    .expect("ping timed out")
    .expect("ping request failed");

    assert_eq!(response, 42);

    // Verify edge discovered its real net_id (should be 1, first allocated)
    let state = edge_stack.manage_profile(|im| im.interface_state(()));
    match state {
        Some(InterfaceState::Active { net_id, node_id }) => {
            assert_eq!(net_id, 1, "edge should have discovered net_id=1");
            assert_eq!(node_id, 2, "edge node_id should be EDGE_NODE_ID");
        }
        other => panic!("expected Active state, got {:?}", other),
    }
}

/// After link-local discovery, the edge can communicate normally using
/// its real net_id — including pinging the router at the discovered address.
#[tokio::test]
async fn edge_link_local_then_normal_ping() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: RouterStack = RouterStack::new();
    let (edge_stack, edge_queue) = make_edge_stack();

    let (e_read, r_write) = tokio::io::duplex(8192);
    let (r_read, e_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r_read,
        r_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge_stack.clone(),
        e_read,
        e_write,
        edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: 2,
        },
        None,
        None,
    )
    .await
    .unwrap();

    spawn_router_ping_server(&router_stack);

    // Step 1: link-local ping to discover net_id
    let router_link_local = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };

    let response = timeout(
        Duration::from_secs(5),
        edge_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_link_local, &1, Some("ping")),
    )
    .await
    .expect("link-local ping timed out")
    .expect("link-local ping failed");
    assert_eq!(response, 1);

    // Step 2: now ping the router using the real address
    let router_real_addr = Address {
        network_id: 1,
        node_id: 1,
        port_id: 0,
    };

    let response = timeout(
        Duration::from_secs(5),
        edge_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_real_addr, &99, Some("ping")),
    )
    .await
    .expect("real-address ping timed out")
    .expect("real-address ping failed");
    assert_eq!(response, 99);
}

/// Two edges connected to the same router: one initiates via link-local,
/// the other is bootstrapped normally. They can then ping each other.
#[tokio::test]
async fn link_local_edge_pings_normal_edge() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: RouterStack = RouterStack::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    // Edge1 <-> Router
    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);
    // Edge2 <-> Router
    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r1_read,
        r1_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r2_read,
        r2_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // Edge1: link-local (Active with net_id=0)
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge1_stack.clone(),
        e1_read,
        e1_write,
        edge1_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: 2,
        },
        None,
        None,
    )
    .await
    .unwrap();

    // Edge2: also link-local
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge2_stack.clone(),
        e2_read,
        e2_write,
        edge2_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: 2,
        },
        None,
        None,
    )
    .await
    .unwrap();

    spawn_router_ping_server(&router_stack);
    common::spawn_ping_server(&edge1_stack);
    common::spawn_ping_server(&edge2_stack);

    // Edge1 discovers its net_id via link-local ping to router
    let router_link_local = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };

    let r = timeout(
        Duration::from_secs(5),
        edge1_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_link_local, &10, Some("ping")),
    )
    .await
    .expect("edge1 link-local ping timed out")
    .expect("edge1 link-local ping failed");
    assert_eq!(r, 10);

    // Edge2 discovers its net_id via link-local ping to router
    let r = timeout(
        Duration::from_secs(5),
        edge2_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_link_local, &20, Some("ping")),
    )
    .await
    .expect("edge2 link-local ping timed out")
    .expect("edge2 link-local ping failed");
    assert_eq!(r, 20);

    // Both edges should now have real net_ids
    let edge1_net = match edge1_stack.manage_profile(|im| im.interface_state(())) {
        Some(InterfaceState::Active { net_id, .. }) => net_id,
        other => panic!("edge1 expected Active, got {:?}", other),
    };
    let edge2_net = match edge2_stack.manage_profile(|im| im.interface_state(())) {
        Some(InterfaceState::Active { net_id, .. }) => net_id,
        other => panic!("edge2 expected Active, got {:?}", other),
    };
    assert_ne!(edge1_net, edge2_net, "edges should have different net_ids");
    assert_ne!(edge1_net, 0);
    assert_ne!(edge2_net, 0);

    // Edge1 pings Edge2 through the router
    let edge2_addr = Address {
        network_id: edge2_net,
        node_id: 2,
        port_id: 0,
    };

    // Give a moment for routing to stabilize
    sleep(Duration::from_millis(50)).await;

    let r = common::ping_with_retry(&edge1_stack, edge2_addr, 77).await;
    assert_eq!(r, 77);
}
