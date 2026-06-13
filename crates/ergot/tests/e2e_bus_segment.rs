//! E2E test: multiple devices on a single shared bus segment.
//!
//! Uses the [`Bus`] mock to simulate a shared medium (ESP-NOW, CAN FD,
//! RS-485) where one router and multiple edges share a single net_id.
//!
//! Topology:
//! ```text
//!           ┌─── Bus (shared medium, net_id=1) ───┐
//!           │                                      │
//!     Root Router                            Edge A, Edge B
//!       (1.1)                              (1.A)     (1.B)
//! ```
//!
//! Edge A and Edge B claim unique node_ids via the address claim protocol,
//! then communicate with the root and with each other through the router.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::{pin::pin, time::Duration};

use common::Bus;
use ergot::{
    Address,
    interface_manager::{
        FrameProcessor, InterfaceState, Interface, Profile,
        profiles::{
            direct_edge::{DirectEdge, EdgeFrameProcessor},
            router::{Router, RouterFrameProcessor},
        },
        utils::{framed_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    well_known::{
        AddressClaimRequest, ErgotAddressClaimEndpoint, ErgotPingEndpoint,
    },
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

// ---- Types ----

// Interface type for bus devices using framed_stream Sink
struct BusInterface;
impl Interface for BusInterface {
    type Sink = framed_stream::Sink<ergot::interface_manager::utils::std::StdQueue>;
}

type BusRouterStack = ArcNetStack<
    CriticalSectionRawMutex,
    Router<BusInterface, rand::rngs::StdRng, 4, 4, 16>,
>;

type BusEdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<BusInterface>>;

// ---- Helpers ----

fn make_bus_edge(queue: &ergot::interface_manager::utils::std::StdQueue, mtu: u16) -> BusEdgeStack {
    BusEdgeStack::new_with_profile(DirectEdge::new_target(
        framed_stream::Sink::new_from_handle(queue.clone(), mtu),
    ))
}

/// Spawn a bus TX worker: reads framed packets from the queue and sends to bus tap.
fn spawn_bus_tx(tap: common::BusTap, queue: ergot::interface_manager::utils::std::StdQueue) {
    // Leak the queue so the consumer has a 'static lifetime for tokio::spawn
    let queue: &'static _ = Box::leak(Box::new(queue));
    let consumer = queue.framed_consumer();
    tokio::spawn(async move {
        loop {
            let grant = consumer.wait_read().await;
            tap.send(&grant);
            grant.release();
        }
    });
}

/// Spawn a bus RX worker for a router: reads frames from bus tap and feeds to process_frame.
fn spawn_bus_router_rx(
    mut tap: common::BusTap,
    stack: &BusRouterStack,
    ident: u8,
    net_id: u16,
) {
    let stack = stack.clone();
    tokio::spawn(async move {
        let mut processor = RouterFrameProcessor::new(net_id);
        loop {
            let data = tap.recv().await;
            if data.is_empty() {
                break;
            }
            let changed = processor.process_frame(&data, &stack, ident);
            if changed {
                log::debug!("[bus router rx] state changed on ident={}", ident);
            }
        }
    });
}

/// Spawn a bus RX worker for an edge: reads frames from bus tap and feeds to process_frame.
fn spawn_bus_edge_rx(mut tap: common::BusTap, stack: &BusEdgeStack) {
    let stack = stack.clone();
    tokio::spawn(async move {
        let mut processor = EdgeFrameProcessor::new();
        loop {
            let data = tap.recv().await;
            if data.is_empty() {
                break;
            }
            let changed = processor.process_frame(&data, &stack, ());
            if changed {
                log::debug!("[bus edge rx] state changed");
            }
        }
    });
}

fn spawn_edge_ping_server(stack: &BusEdgeStack) {
    let stack = stack.clone();
    tokio::spawn(async move {
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
    });
}

// ---- Tests ----

/// Two edges claim different node_ids on the same bus, then ping the router.
#[tokio::test]
async fn two_edges_on_shared_bus_claim_and_ping() {
    let _ = env_logger::builder().is_test(true).try_init();

    let bus = Bus::new();
    const MTU: u16 = 250;

    // ---- Root Router ----
    let router_queue = new_std_queue(4096);
    let router_stack: BusRouterStack = BusRouterStack::new_with_profile({
        Router::new_std()
    });

    let router_sink = framed_stream::Sink::new_from_handle(router_queue.clone(), MTU);
    let router_ident = router_stack
        .manage_profile(|im| im.register_interface(router_sink))
        .unwrap();
    let router_net_id = router_stack
        .manage_profile(|im| im.net_id_of(router_ident))
        .unwrap();

    let router_tap = bus.tap();
    spawn_bus_tx(bus.tap(), router_queue.clone());
    spawn_bus_router_rx(router_tap, &router_stack, router_ident, router_net_id);

    // Router services
    tokio::spawn({
        let s = router_stack.clone();
        async move { s.services().ping_handler::<4>().await }
    });
    tokio::spawn({
        let s = router_stack.clone();
        async move { s.services().address_claim_handler::<4>().await }
    });

    // Let services register
    sleep(Duration::from_millis(50)).await;

    // ---- Edge A (candidate node_id=30) ----
    let edge_a_queue = new_std_queue(4096);
    let edge_a_stack = make_bus_edge(&edge_a_queue, MTU);
    // Start with link-local + candidate node_id
    edge_a_stack
        .manage_profile(|im| {
            im.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: 0,
                    node_id: 30,
                },
            )
        })
        .unwrap();

    spawn_bus_tx(bus.tap(), edge_a_queue.clone());
    spawn_bus_edge_rx(bus.tap(), &edge_a_stack);
    spawn_edge_ping_server(&edge_a_stack);

    // ---- Edge B (candidate node_id=50) ----
    let edge_b_queue = new_std_queue(4096);
    let edge_b_stack = make_bus_edge(&edge_b_queue, MTU);
    edge_b_stack
        .manage_profile(|im| {
            im.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: 0,
                    node_id: 50,
                },
            )
        })
        .unwrap();

    spawn_bus_tx(bus.tap(), edge_b_queue.clone());
    spawn_bus_edge_rx(bus.tap(), &edge_b_stack);
    spawn_edge_ping_server(&edge_b_stack);

    let link_local_router = Address {
        network_id: 0,
        node_id: 1,
        port_id: 0,
    };

    // ---- Edge A claims node_id=30 ----
    let claim_a = timeout(
        Duration::from_secs(5),
        edge_a_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local_router,
            &AddressClaimRequest {
                candidate_node_id: 30,
                nonce: 0xAAAA,
            },
            None,
        ),
    )
    .await
    .expect("edge A claim timed out")
    .expect("edge A claim request failed");

    let grant_a = claim_a.expect("edge A claim should be granted");
    assert_eq!(grant_a.assignment.node_id, 30);
    assert_eq!(grant_a.assignment.net_id, router_net_id);

    // Update edge A to use granted address
    edge_a_stack
        .manage_profile(|im| {
            im.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: grant_a.assignment.net_id,
                    node_id: grant_a.assignment.node_id,
                },
            )
        })
        .unwrap();

    // ---- Edge B claims node_id=50 ----
    let claim_b = timeout(
        Duration::from_secs(5),
        edge_b_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local_router,
            &AddressClaimRequest {
                candidate_node_id: 50,
                nonce: 0xBBBB,
            },
            None,
        ),
    )
    .await
    .expect("edge B claim timed out")
    .expect("edge B claim request failed");

    let grant_b = claim_b.expect("edge B claim should be granted");
    assert_eq!(grant_b.assignment.node_id, 50);
    assert_eq!(grant_b.assignment.net_id, router_net_id);

    edge_b_stack
        .manage_profile(|im| {
            im.set_interface_state(
                (),
                InterfaceState::Active {
                    net_id: grant_b.assignment.net_id,
                    node_id: grant_b.assignment.node_id,
                },
            )
        })
        .unwrap();

    // ---- Both edges have unique addresses on the same net_id ----
    let edge_a_addr = Address {
        network_id: router_net_id,
        node_id: 30,
        port_id: 0,
    };
    let edge_b_addr = Address {
        network_id: router_net_id,
        node_id: 50,
        port_id: 0,
    };

    // ---- Router pings both edges ----
    let r = common::ping_with_retry(&router_stack, edge_a_addr, 111).await;
    assert_eq!(r, 111, "router → edge A ping should work");

    let r = common::ping_with_retry(&router_stack, edge_b_addr, 222).await;
    assert_eq!(r, 222, "router → edge B ping should work");
}

/// Conflict: two edges try to claim the same node_id on the same bus.
#[tokio::test]
async fn bus_claim_conflict_same_segment() {
    let _ = env_logger::builder().is_test(true).try_init();

    let bus = Bus::new();
    const MTU: u16 = 250;

    // ---- Root Router ----
    let router_queue = new_std_queue(4096);
    let router_stack: BusRouterStack = BusRouterStack::new_with_profile(Router::new_std());

    let router_sink = framed_stream::Sink::new_from_handle(router_queue.clone(), MTU);
    let router_ident = router_stack
        .manage_profile(|im| im.register_interface(router_sink))
        .unwrap();
    let router_net_id = router_stack
        .manage_profile(|im| im.net_id_of(router_ident))
        .unwrap();

    spawn_bus_tx(bus.tap(), router_queue.clone());
    spawn_bus_router_rx(bus.tap(), &router_stack, router_ident, router_net_id);

    tokio::spawn({
        let s = router_stack.clone();
        async move { s.services().address_claim_handler::<4>().await }
    });
    sleep(Duration::from_millis(50)).await;

    // ---- Edge A claims node_id=42 ----
    let edge_a_queue = new_std_queue(4096);
    let edge_a_stack = make_bus_edge(&edge_a_queue, MTU);
    edge_a_stack
        .manage_profile(|im| {
            im.set_interface_state((), InterfaceState::Active { net_id: 0, node_id: 42 })
        })
        .unwrap();
    spawn_bus_tx(bus.tap(), edge_a_queue.clone());
    spawn_bus_edge_rx(bus.tap(), &edge_a_stack);

    let link_local = Address { network_id: 0, node_id: 1, port_id: 0 };

    let r = timeout(
        Duration::from_secs(5),
        edge_a_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 42, nonce: 0x1111 },
            None,
        ),
    )
    .await.unwrap().unwrap();
    assert!(r.is_ok(), "edge A should claim successfully");

    // ---- Edge B tries same node_id=42 with different nonce ----
    let edge_b_queue = new_std_queue(4096);
    let edge_b_stack = make_bus_edge(&edge_b_queue, MTU);
    edge_b_stack
        .manage_profile(|im| {
            im.set_interface_state((), InterfaceState::Active { net_id: 0, node_id: 42 })
        })
        .unwrap();
    spawn_bus_tx(bus.tap(), edge_b_queue.clone());
    spawn_bus_edge_rx(bus.tap(), &edge_b_stack);

    let r = timeout(
        Duration::from_secs(5),
        edge_b_stack.endpoints().request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest { candidate_node_id: 42, nonce: 0x2222 },
            None,
        ),
    )
    .await.unwrap().unwrap();

    assert!(r.is_err(), "edge B should get conflict");
    assert_eq!(
        r.unwrap_err(),
        ergot::interface_manager::AddressClaimError::Conflict
    );
}
