//! End-to-end test for UDP link-local addressing — regression for the
//! peer-learning deadlock.
//!
//! Reproduces the exact host<->device scenario: an edge initiates over a
//! *connected* UDP socket using link-local addressing (net_id=0), while the
//! device runs a `Router` over an *unconnected* socket.
//!
//! Before the fix, the edge's tx worker decided whether to learn a peer from
//! an `is_target` heuristic keyed on net_id (net_id==0 => wait to learn a peer
//! before sending). A link-local edge has net_id=0 yet, over a *connected*
//! socket, must transmit first — so it waited forever for a peer it could only
//! learn from an inbound datagram that never came. The `Router`, in turn, had
//! no peer-learning path and could not reply to whoever reached it. This test
//! times out on that code and passes once peer-learning is decided from the
//! socket's connectedness instead.

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
use tokio::time::timeout;

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
        edge_stack.endpoints().request::<ErgotPingEndpoint>(
            router_link_local,
            &42,
            Some("ping"),
        ),
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
