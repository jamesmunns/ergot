//! End-to-end tests for UDP peer-learning — regression for the deadlock that
//! followed from deciding peer discovery by addressing role instead of by the
//! socket's connectedness.
//!
//! Before the fix, the tx worker decided whether to learn a peer from an
//! `is_target` heuristic keyed on net_id (net_id==0 => wait to learn a peer
//! before sending), and the `Router` had no peer-learning path at all. A
//! link-local edge has net_id=0 yet, over a *connected* socket, must transmit
//! first — so it waited forever for a peer it could only learn from an inbound
//! datagram that never came, and the `Router` could not reply to whoever
//! reached it.
//!
//! The two tests cover both branches of the connectedness decision:
//! - [`edge_initiates_link_local_ping_over_udp`]: a *connected* edge initiates,
//!   an *unconnected* `Router` learns its peer. This is the actual regression —
//!   it deadlocks on the old `is_target` heuristic and only passes once
//!   peer-learning is decided from connectedness.
//! - [`controller_initiates_ping_to_unconnected_edge_target`]: a *connected*
//!   controller initiates, an *unconnected* edge target learns its peer. The
//!   old net_id heuristic also handled this case (a net_id=0 target was always
//!   treated as `is_target`), so it does not reproduce the bug; it guards the
//!   edge learn-peer branch so the connectedness rule can't silently break the
//!   server-side edge in a future refactor.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::pin::pin;
use std::time::Duration;

use ergot::{
    Address,
    interface_manager::{InterfaceState, Profile},
    toolkits::tokio_udp::{self, EdgeStack, RouterStack},
    well_known::ErgotPingEndpoint,
};
use tokio::net::UdpSocket;
use tokio::time::{sleep, timeout};

/// Spawn a ping server (echoes the `u32` it receives) on the router stack.
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

/// An edge on a *connected* UDP socket initiates a link-local ping to a
/// `Router` on an *unconnected* socket. The edge must send first; the router
/// must learn the edge's address from that first datagram to reply.
#[tokio::test]
async fn edge_initiates_link_local_ping_over_udp() {
    let _ = env_logger::builder().is_test(true).try_init();

    // Router side: bind an *unconnected* socket on an ephemeral port and run a
    // Router over it. It will learn its peer from the first datagram received.
    let router_stack: RouterStack = RouterStack::new();
    let router_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let router_addr = router_sock.local_addr().unwrap();
    tokio_udp::register_router_interface(&router_stack, router_sock, 512, 4096)
        .await
        .expect("router registration");
    spawn_router_ping_server(&router_stack);

    // Edge side: bind a socket, `connect()` it to the router (so it is a
    // *connected* socket), and register as a link-local target (net_id=0).
    let edge_queue = tokio_udp::new_std_queue(4096);
    let edge_stack: EdgeStack = tokio_udp::new_target_stack(&edge_queue, 512);
    let edge_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    edge_sock
        .connect(router_addr)
        .await
        .expect("edge connect to router");
    tokio_udp::register_edge_target_interface(&edge_stack, edge_sock, &edge_queue, None, None)
        .await
        .expect("edge registration");

    // The edge initiates first, addressing the router link-local
    // (net_id=0, node_id=1=CENTRAL_NODE_ID, port wildcard → find by key).
    let router_link_local = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };
    let response = timeout(
        Duration::from_secs(3),
        edge_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_link_local, &42, Some("ping")),
    )
    .await
    .expect("ping timed out — edge never transmitted (UDP peer-learning deadlock)")
    .expect("ping request failed");
    assert_eq!(response, 42);

    // The edge should have discovered its real net_id from the router's reply.
    match edge_stack.manage_profile(|im| im.interface_state(())) {
        Some(InterfaceState::Active { net_id, node_id }) => {
            assert_eq!(net_id, 1, "edge should have discovered net_id=1");
            assert_eq!(node_id, 2, "edge node_id should be EDGE_NODE_ID");
        }
        other => panic!("expected Active state, got {other:?}"),
    }
}

/// Spawn a ping server (echoes the `u32` it receives) on an edge stack.
fn spawn_edge_ping_server(stack: &EdgeStack) {
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

/// Ping with retries: the first request bootstraps the target's net_id and may
/// land before its server has attached, so retry a few times before giving up.
async fn ping_with_retry(stack: &EdgeStack, addr: Address, val: u32) -> u32 {
    for _ in 0..20 {
        let result = timeout(
            Duration::from_millis(500),
            stack
                .endpoints()
                .request::<ErgotPingEndpoint>(addr, &val, Some("ping")),
        )
        .await;
        if let Ok(Ok(v)) = result {
            return v;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("ping failed after retries — UDP peer-learning likely deadlocked");
}

/// A controller on a *connected* UDP socket initiates a ping to an edge
/// *target* on an *unconnected* socket. The target must learn the controller's
/// address from the first datagram it receives to reply via `send_to`.
///
/// This is the mirror of [`edge_initiates_link_local_ping_over_udp`]: it
/// exercises the *edge* learn-peer path (`learn_peer = true` over an
/// unconnected socket) plus a connected controller (`learn_peer = false`),
/// rather than the router one. Unlike the link-local test it does not
/// reproduce the original deadlock — the old net_id heuristic treated a
/// net_id=0 target as `is_target` and so also learned a peer here. Its job is
/// to pin the edge learn-peer branch in place so the switch to a
/// connectedness-based decision can't silently break the server-side edge.
#[tokio::test]
async fn controller_initiates_ping_to_unconnected_edge_target() {
    let _ = env_logger::builder().is_test(true).try_init();

    // Target side: bind an *unconnected* socket and register as a link-local
    // edge target. It learns its peer from the first datagram the controller
    // sends, then replies via `send_to`.
    let target_queue = tokio_udp::new_std_queue(4096);
    let target_stack: EdgeStack = tokio_udp::new_target_stack(&target_queue, 512);
    let target_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target_sock.local_addr().unwrap();
    tokio_udp::register_edge_target_interface(
        &target_stack,
        target_sock,
        &target_queue,
        None,
        None,
    )
    .await
    .expect("target registration");
    spawn_edge_ping_server(&target_stack);

    // Controller side: bind a socket, `connect()` it to the target (so it is a
    // *connected* socket), and register as a controller (net_id=1, central).
    let ctrl_queue = tokio_udp::new_std_queue(4096);
    let ctrl_stack: EdgeStack = tokio_udp::new_controller_stack(&ctrl_queue, 512);
    let ctrl_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    ctrl_sock
        .connect(target_addr)
        .await
        .expect("controller connect to target");
    tokio_udp::register_edge_controller_interface(&ctrl_stack, ctrl_sock, &ctrl_queue, None, None)
        .await
        .expect("controller registration");

    // The controller initiates first, addressing the target at its assigned
    // address (net_id=1, node_id=2=EDGE_NODE_ID). The first ping bootstraps the
    // target's net_id.
    let target = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0,
    };
    let response = ping_with_retry(&ctrl_stack, target, 42).await;
    assert_eq!(response, 42);

    // The target should have adopted the controller's net_id.
    match target_stack.manage_profile(|im| im.interface_state(())) {
        Some(InterfaceState::Active { net_id, node_id }) => {
            assert_eq!(net_id, 1, "target should have adopted net_id=1");
            assert_eq!(node_id, 2, "target node_id should be EDGE_NODE_ID");
        }
        other => panic!("expected Active state, got {other:?}"),
    }
}
