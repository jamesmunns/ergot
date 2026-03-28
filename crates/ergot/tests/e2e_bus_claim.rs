//! End-to-end tests for bus-style address claim protocol.
//!
//! Simulates bus-style interfaces where multiple edge devices share a
//! network segment and need to claim unique node_ids via the address
//! claim endpoint before communicating normally.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::{pin::pin, time::Duration};

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
    well_known::{
        AddressClaimRequest, ErgotAddressClaimEndpoint, ErgotPingEndpoint,
    },
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

/// Router with C=16 bus claim slots.
type BusRouterStack = ArcNetStack<
    CriticalSectionRawMutex,
    Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64, 16>,
>;

fn spawn_ping_server_on_router(stack: &BusRouterStack) {
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

fn spawn_claim_handler(stack: &BusRouterStack) {
    tokio::spawn({
        let stack = stack.clone();
        async move {
            stack.services().address_claim_handler::<4>().await;
        }
    });
}

/// A bus edge claims a node_id, then pings the router using its assigned address.
#[tokio::test]
async fn bus_edge_claims_and_pings() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: BusRouterStack = BusRouterStack::new();
    let (edge_stack, edge_queue) = common::make_edge_stack();

    let (e_read, r_write) = tokio::io::duplex(8192);
    let (r_read, e_write) = tokio::io::duplex(8192);

    // Register router interface
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

    // Edge starts with candidate node_id=47 (bus-style, not EDGE_NODE_ID)
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge_stack.clone(),
        e_read,
        e_write,
        edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: 47,
        },
        None,
        None,
    )
    .await
    .unwrap();

    spawn_claim_handler(&router_stack);
    spawn_ping_server_on_router(&router_stack);

    // Edge sends address claim request via link-local
    let router_link_local = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };

    let claim_response = timeout(
        Duration::from_secs(5),
        edge_stack
            .endpoints()
            .request::<ErgotAddressClaimEndpoint>(
                router_link_local,
                &AddressClaimRequest {
                    candidate_node_id: 47,
                    nonce: 0xDEAD,
                },
                None,
            ),
    )
    .await
    .expect("claim timed out")
    .expect("claim request failed");

    let granted = claim_response.expect("claim should be granted");
    assert_eq!(granted.assignment.node_id, 47);
    assert_eq!(granted.assignment.net_id, 1);

    // Update edge state to use the granted address
    edge_stack.manage_profile(|im| {
        im.set_interface_state(
            (),
            InterfaceState::Active {
                net_id: granted.assignment.net_id,
                node_id: granted.assignment.node_id,
            },
        )
    }).unwrap();

    // Now ping the router using the real address
    let router_addr = Address {
        network_id: 1,
        node_id: 1,
        port_id: 0,
    };

    let response = timeout(
        Duration::from_secs(5),
        edge_stack
            .endpoints()
            .request::<ErgotPingEndpoint>(router_addr, &42, Some("ping")),
    )
    .await
    .expect("ping timed out")
    .expect("ping failed");

    assert_eq!(response, 42);
}

/// Two edges claim different node_ids on the same bus.
#[tokio::test]
async fn two_edges_claim_different_node_ids() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: BusRouterStack = BusRouterStack::new();
    let (edge1_stack, edge1_queue) = common::make_edge_stack();
    let (edge2_stack, edge2_queue) = common::make_edge_stack();

    // Edge1 <-> Router
    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);
    // Edge2 <-> Router
    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(), r1_read, r1_write, 512, 4096, None, None,
    ).await.unwrap();
    tokio_cobs_stream::register_router(
        router_stack.clone(), r2_read, r2_write, 512, 4096, None, None,
    ).await.unwrap();

    // Edge1: candidate node_id=10
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge1_stack.clone(), e1_read, e1_write, edge1_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active { net_id: 0, node_id: 10 },
        None, None,
    ).await.unwrap();

    // Edge2: candidate node_id=20
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge2_stack.clone(), e2_read, e2_write, edge2_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active { net_id: 0, node_id: 20 },
        None, None,
    ).await.unwrap();

    spawn_claim_handler(&router_stack);
    spawn_ping_server_on_router(&router_stack);
    common::spawn_ping_server(&edge1_stack);
    common::spawn_ping_server(&edge2_stack);

    let link_local = Address { network_id: 0, node_id: 1, port_id: 0 };

    // Edge1 claims node_id=10
    let r1 = timeout(Duration::from_secs(5),
        edge1_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 10, nonce: 0xAA },
            None,
        ),
    ).await.unwrap().unwrap();
    let g1 = r1.expect("edge1 claim should succeed");
    assert_eq!(g1.assignment.node_id, 10);

    // Edge2 claims node_id=20
    let r2 = timeout(Duration::from_secs(5),
        edge2_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 20, nonce: 0xBB },
            None,
        ),
    ).await.unwrap().unwrap();
    let g2 = r2.expect("edge2 claim should succeed");
    assert_eq!(g2.assignment.node_id, 20);

    // Update edge states
    edge1_stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Active {
            net_id: g1.assignment.net_id, node_id: g1.assignment.node_id,
        })
    }).unwrap();
    edge2_stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Active {
            net_id: g2.assignment.net_id, node_id: g2.assignment.node_id,
        })
    }).unwrap();

    // Edge1 pings edge2 through router
    let edge2_addr = Address {
        network_id: g2.assignment.net_id,
        node_id: g2.assignment.node_id,
        port_id: 0,
    };

    sleep(Duration::from_millis(50)).await;

    let r = common::ping_with_retry(&edge1_stack, edge2_addr, 99).await;
    assert_eq!(r, 99);
}

/// Claiming the same node_id on the same interface with a different nonce returns Conflict.
///
/// A single edge first claims node_id=50 with nonce=0x111, then sends a second
/// claim for the same node_id with a different nonce=0x222 (simulating a
/// different device on the same bus that picked the same candidate).
#[tokio::test]
async fn claim_conflict_different_nonce() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: BusRouterStack = BusRouterStack::new();
    let (edge_stack, edge_queue) = common::make_edge_stack();

    let (e_read, r_write) = tokio::io::duplex(8192);
    let (r_read, e_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(), r_read, r_write, 512, 4096, None, None,
    ).await.unwrap();

    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge_stack.clone(), e_read, e_write, edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active { net_id: 0, node_id: 50 },
        None, None,
    ).await.unwrap();

    spawn_claim_handler(&router_stack);

    let link_local = Address { network_id: 0, node_id: 1, port_id: 0 };

    // First claim succeeds
    let r1 = timeout(Duration::from_secs(5),
        edge_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 50, nonce: 0x111 },
            None,
        ),
    ).await.unwrap().unwrap();
    assert!(r1.is_ok(), "first claim should get Granted");

    // Second claim with same candidate but different nonce — Conflict
    let r2 = timeout(Duration::from_secs(5),
        edge_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 50, nonce: 0x222 },
            None,
        ),
    ).await.unwrap().unwrap();
    assert!(r2.is_err(), "second claim with different nonce should get Conflict");

    let err = r2.unwrap_err();
    assert_eq!(err, ergot::interface_manager::AddressClaimError::Conflict);
}

/// Duplicate claim with same nonce returns the existing assignment.
#[tokio::test]
async fn duplicate_claim_same_nonce() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: BusRouterStack = BusRouterStack::new();
    let (edge_stack, edge_queue) = common::make_edge_stack();

    let (e_read, r_write) = tokio::io::duplex(8192);
    let (r_read, e_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(), r_read, r_write, 512, 4096, None, None,
    ).await.unwrap();

    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge_stack.clone(), e_read, e_write, edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active { net_id: 0, node_id: 77 },
        None, None,
    ).await.unwrap();

    spawn_claim_handler(&router_stack);

    let link_local = Address { network_id: 0, node_id: 1, port_id: 0 };
    let req = AddressClaimRequest { candidate_node_id: 77, nonce: 0xCAFE };

    // First claim
    let r1 = timeout(Duration::from_secs(5),
        edge_stack.endpoints().request::<ErgotAddressClaimEndpoint>(link_local, &req, None),
    ).await.unwrap().unwrap();
    let g1 = r1.expect("first claim should succeed");

    // Second claim — same nonce, should return existing assignment
    let r2 = timeout(Duration::from_secs(5),
        edge_stack.endpoints().request::<ErgotAddressClaimEndpoint>(link_local, &req, None),
    ).await.unwrap().unwrap();
    let g2 = r2.expect("duplicate claim should succeed");

    assert_eq!(g1.assignment.node_id, g2.assignment.node_id);
    assert_eq!(g1.assignment.net_id, g2.assignment.net_id);
}
