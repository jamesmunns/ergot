//! E2E test: bridge with link-local upstream, seed routing, and device discovery.
//!
//! Topology (mirrors the ESP-NOW demo):
//! ```text
//! Host (Edge) ←→ Bridge (Router, seed client) ←upstream→ Root (Router, seed router)
//! ```
//!
//! Tests:
//! 1. Bridge upstream starts link-local (net_id=0), discovers net_id from root's response
//! 2. Bridge requests seed net_id from root for downstream interface
//! 3. Host discovers both bridge and root device info through the bridge
//! 4. Host pings root through the bridge
//! 5. Bridge upstream net_id stays stable despite transit frames (regression for
//!    EdgeFrameProcessor re-discovery bug)

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
        profiles::{
            direct_edge::EdgeFrameProcessor,
            router::{Router, UPSTREAM_IDENT},
        },
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::{
        ArcNetStack,
        services::{bridge_seed_assign, bridge_seed_refresh},
    },
    well_known::DeviceInfo,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

type RootStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type BridgeStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

#[tokio::test]
async fn host_discovers_and_pings_through_bridge() {
    let _ = env_logger::builder().is_test(true).try_init();

    // ========== Create stacks ==========
    let root_stack: RootStack = RootStack::new();

    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let (host_stack, host_queue) = make_edge_stack();

    // ========== Wire up duplex pipes ==========
    let (bridge_up_read, root_d_write) = tokio::io::duplex(8192);
    let (root_d_read, bridge_up_write) = tokio::io::duplex(8192);
    let (host_read, bridge_d_write) = tokio::io::duplex(8192);
    let (bridge_d_read, host_write) = tokio::io::duplex(8192);

    // ========== Root services ==========
    tokio::spawn({
        let s = root_stack.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });
    tokio::spawn({
        let s = root_stack.clone();
        async move { s.services().ping_handler::<4>().await }
    });
    tokio::spawn({
        let s = root_stack.clone();
        async move {
            s.services()
                .device_info_handler::<4>(&DeviceInfo {
                    name: Some("Root".try_into().unwrap()),
                    description: Some("Root Router".try_into().unwrap()),
                    unique_id: 1,
                })
                .await
        }
    });

    // ========== Register root downstream ==========
    tokio_cobs_stream::register_router(
        root_stack.clone(),
        root_d_read,
        root_d_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // ========== Register bridge upstream (link-local) ==========
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

    // Verify upstream started link-local
    let upstream_state = bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT));
    assert!(
        matches!(upstream_state, Some(InterfaceState::Active { net_id: 0, .. })),
        "upstream should start link-local, got {:?}",
        upstream_state
    );

    // ========== Register bridge downstream (pending, no net_id) ==========
    let bridge_d_ident = tokio_cobs_stream::register_bridge_downstream(
        bridge_stack.clone(),
        bridge_d_read,
        bridge_d_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .expect("bridge downstream registration");

    // ========== Bridge services ==========
    tokio::spawn({
        let s = bridge_stack.clone();
        async move { s.services().ping_handler::<4>().await }
    });
    tokio::spawn({
        let s = bridge_stack.clone();
        async move {
            s.services()
                .device_info_handler::<4>(&DeviceInfo {
                    name: Some("Bridge".try_into().unwrap()),
                    description: Some("Test Bridge".try_into().unwrap()),
                    unique_id: 2,
                })
                .await
        }
    });

    // Let services register sockets
    sleep(Duration::from_millis(50)).await;

    // Bootstrap the upstream identity before asking for a lease. A source
    // network of zero is deliberately rejected by the seed client.
    let _ = timeout(
        Duration::from_millis(500),
        root_stack.endpoints().request::<ergot::well_known::ErgotPingEndpoint>(
            Address {
                network_id: 1,
                node_id: 2,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    for _ in 0..20 {
        if matches!(
            bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT)),
            Some(InterfaceState::Active { net_id: 1, .. })
        ) {
            break;
        }
        sleep(Duration::from_millis(25)).await;
    }

    // ========== Bridge: seed assign via link-local upstream ==========
    let lease = timeout(
        Duration::from_secs(5),
        bridge_seed_assign(&bridge_stack, UPSTREAM_IDENT, bridge_d_ident),
    )
    .await
    .expect("seed assign timed out")
    .expect("seed assign failed");

    let seed_net_id = lease.net_id;
    assert_ne!(seed_net_id, 0, "seed net_id should be non-zero");

    // Verify upstream discovered real net_id
    let upstream_net_id = match bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT))
    {
        Some(InterfaceState::Active { net_id, .. }) => {
            assert_ne!(net_id, 0, "upstream should have discovered real net_id");
            net_id
        }
        other => panic!("upstream should be Active, got {:?}", other),
    };

    // ========== Register host edge (link-local) ==========
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        host_stack.clone(),
        host_read,
        host_write,
        host_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 0,
            node_id: ergot::prelude::EDGE_NODE_ID,
        },
        None,
        None,
    )
    .await
    .unwrap();

    spawn_ping_server(&host_stack);

    // Bootstrap host: bridge pings host to trigger net_id discovery
    let host_addr = Address {
        network_id: seed_net_id,
        node_id: 2,
        port_id: 0,
    };
    ping_with_retry(&bridge_stack, host_addr, 0).await;
    wait_active(&host_stack).await;

    // ========== Host discovers devices ==========
    let devices = timeout(
        Duration::from_secs(5),
        host_stack.discovery().discover(10, Duration::from_secs(2)),
    )
    .await
    .expect("discovery timed out");

    let root_found = devices.iter().any(|d| d.info.unique_id == 1);
    let bridge_found = devices.iter().any(|d| d.info.unique_id == 2);
    assert!(
        root_found,
        "host should discover root through bridge, found: {:?}",
        devices.iter().map(|d| &d.info.name).collect::<Vec<_>>()
    );
    assert!(
        bridge_found,
        "host should discover bridge, found: {:?}",
        devices.iter().map(|d| &d.info.name).collect::<Vec<_>>()
    );

    // ========== Verify bridge upstream is still correct ==========
    // Transit frames (device info responses with dst matching host's net_id)
    // must NOT corrupt the bridge upstream's net_id.
    let upstream_now = bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT));
    match upstream_now {
        Some(InterfaceState::Active { net_id, .. }) => {
            assert_eq!(
                net_id, upstream_net_id,
                "upstream net_id should be stable (was {upstream_net_id}, now {net_id})"
            );
        }
        other => panic!("upstream should still be Active, got {:?}", other),
    }

    // ========== Seed refresh ==========
    let refreshed = timeout(
        Duration::from_secs(5),
        bridge_seed_refresh(&bridge_stack, &lease),
    )
    .await
    .expect("refresh timed out")
    .expect("refresh failed");
    assert_eq!(refreshed.net_id, seed_net_id);

    // Upstream still stable
    let upstream_after = bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT));
    match upstream_after {
        Some(InterfaceState::Active { net_id, .. }) => {
            assert_eq!(net_id, upstream_net_id, "upstream should be stable after refresh");
        }
        other => panic!("upstream should be Active after refresh, got {:?}", other),
    }
}
