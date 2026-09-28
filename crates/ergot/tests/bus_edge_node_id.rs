//! A device on a shared bus keeps the node_id it owns.
//!
//! On a point-to-point link an edge is always [`EDGE_NODE_ID`], so the
//! discovery and liveness paths used to (re)activate interfaces with that
//! constant. On a bus the node_id is a claim candidate or a claimed address,
//! and replacing it with `EDGE_NODE_ID` silently moves the device onto a
//! reserved address that every other bus device would share.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::time::Duration;

use common::Bus;
use ergot::{
    Address, AnyAllAppendix, FrameKind, Header, Key, TrafficClass,
    interface_manager::{
        FrameProcessor, Interface, InterfaceState, LivenessConfig, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::{
            direct_edge::{DirectEdge, EDGE_NODE_ID, EdgeFrameProcessor},
            router::{Router, RouterFrameProcessor, UPSTREAM_IDENT},
        },
        transports::tokio_cobs_stream,
        utils::{
            cobs_stream, framed_stream,
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::{ArcNetStack, services::bus_claim_with_retry},
    well_known::ErgotPingEndpoint,
    wire_frames::{de_frame, encode_frame_ty},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

struct BusInterface;
impl Interface for BusInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}

type BusEdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<BusInterface>>;
type BusRouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<BusInterface, rand::rngs::StdRng, 4, 4, 16>>;

const BUS_NET: u16 = 5;
const CLAIMED: u8 = 30;

fn bus_edge(queue: &StdQueue) -> BusEdgeStack {
    BusEdgeStack::new_with_profile(DirectEdge::new_target(
        framed_stream::Sink::new_from_handle(queue.clone(), 250),
    ))
}

fn edge_state(stack: &BusEdgeStack) -> Option<InterfaceState> {
    stack.manage_profile(|im| im.interface_state(()))
}

fn set_edge_state(stack: &BusEdgeStack, state: InterfaceState) {
    stack
        .manage_profile(|im| im.set_interface_state((), state))
        .unwrap();
}

/// An encoded frame from the bus router to `dst`.
fn frame_to(dst: Address) -> Vec<u8> {
    let any_all = [0, 255].contains(&dst.port_id);
    let hdr = Header {
        src: Address {
            network_id: BUS_NET,
            node_id: 1,
            port_id: 10,
        },
        dst,
        any_all: any_all.then_some(AnyAllAppendix {
            key: Key([0; 8]),
            nash: None,
        }),
        kind: FrameKind::TOPIC_MSG,
        class: TrafficClass::Normal,
        ttl: 15,
    };
    encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &0u8).unwrap()
}

// ---- EdgeFrameProcessor ----

/// Before its claim completes, a bus edge hears the router's broadcasts. Their
/// broadcast leg is addressed to `EDGE_NODE_ID`, which used to donate the net
/// AND move the edge onto node 2.
#[test]
fn preclaim_edge_keeps_candidate_on_router_broadcast() {
    let stack = bus_edge(&new_std_queue(4096));
    set_edge_state(&stack, InterfaceState::link_local(CLAIMED));

    let mut proc = EdgeFrameProcessor::new();
    proc.process_frame(
        &frame_to(Address {
            network_id: BUS_NET,
            node_id: EDGE_NODE_ID,
            port_id: 255,
        }),
        &stack,
        (),
    );

    assert_eq!(
        edge_state(&stack),
        Some(InterfaceState::link_local(CLAIMED)),
        "a broadcast addressed to another node must not change our address"
    );
}

/// A frame addressed to the candidate (e.g. the claim response) donates the
/// net and keeps the candidate as the node_id.
#[test]
fn preclaim_edge_learns_net_from_frame_to_candidate() {
    let stack = bus_edge(&new_std_queue(4096));
    set_edge_state(&stack, InterfaceState::link_local(CLAIMED));

    let mut proc = EdgeFrameProcessor::new();
    let changed = proc.process_frame(
        &frame_to(Address {
            network_id: BUS_NET,
            node_id: CLAIMED,
            port_id: 7,
        }),
        &stack,
        (),
    );

    assert!(changed);
    assert_eq!(
        edge_state(&stack),
        Some(InterfaceState::Active {
            net_id: BUS_NET,
            node_id: CLAIMED
        })
    );
}

/// A liveness timeout sets `Inactive`, which carries no node_id. The first
/// frame afterwards must reactivate the claimed node_id, not `EDGE_NODE_ID`.
#[test]
fn claimed_edge_reactivates_with_its_node_after_liveness_reset() {
    let stack = bus_edge(&new_std_queue(4096));
    let claimed = InterfaceState::Active {
        net_id: BUS_NET,
        node_id: CLAIMED,
    };
    set_edge_state(&stack, claimed);
    let to_us = frame_to(Address {
        network_id: BUS_NET,
        node_id: CLAIMED,
        port_id: 7,
    });

    let mut proc = EdgeFrameProcessor::new();
    proc.process_frame(&to_us, &stack, ());
    assert_eq!(edge_state(&stack), Some(claimed));

    // What the packet worker does on a liveness timeout.
    set_edge_state(&stack, InterfaceState::Inactive);
    FrameProcessor::<BusEdgeStack>::reset(&mut proc);

    let changed = proc.process_frame(&to_us, &stack, ());
    assert!(changed, "reactivation is a state change");
    assert_eq!(edge_state(&stack), Some(claimed));
}

/// Rediscovery after a reset without a state change (sticky net, still
/// Active) must not rewrite the node_id either.
#[test]
fn claimed_edge_keeps_its_node_through_rediscovery() {
    let stack = bus_edge(&new_std_queue(4096));
    let claimed = InterfaceState::Active {
        net_id: BUS_NET,
        node_id: CLAIMED,
    };
    set_edge_state(&stack, claimed);
    let to_us = frame_to(Address {
        network_id: BUS_NET,
        node_id: CLAIMED,
        port_id: 7,
    });

    let mut proc = EdgeFrameProcessor::new();
    proc.process_frame(&to_us, &stack, ());
    FrameProcessor::<BusEdgeStack>::reset(&mut proc);
    proc.process_frame(&to_us, &stack, ());

    assert_eq!(edge_state(&stack), Some(claimed));
}

// ---- Liveness revert to link-local ----

type RootStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type BridgeStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

/// The revert-to-link-local liveness policy keeps the upstream's node_id: a
/// bridge whose upstream sits on a bus at a claimed node_id must not fall
/// back to `EDGE_NODE_ID` when the link goes quiet.
#[tokio::test]
async fn link_local_revert_keeps_the_node_id() {
    let _ = env_logger::builder().is_test(true).try_init();
    let upstream_node = 40;

    let root_stack: RootStack = RootStack::new();
    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));
    let upstream_state = || bridge_stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT));

    let (bridge_up_read, root_d_write) = tokio::io::duplex(8192);
    let (root_d_read, bridge_up_write) = tokio::io::duplex(8192);
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
    tokio_cobs_stream::register_bridge_upstream(
        bridge_stack.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        Some(LivenessConfig { timeout_ms: 500 }),
        None,
    )
    .await
    .unwrap();

    // As if the upstream had claimed a node_id on a bus segment.
    bridge_stack
        .manage_profile(|im| {
            im.set_interface_state(UPSTREAM_IDENT, InterfaceState::link_local(upstream_node))
        })
        .unwrap();

    // A frame addressed to that node_id arms liveness and donates the net.
    let _ = timeout(
        Duration::from_millis(500),
        root_stack.endpoints().request::<ErgotPingEndpoint>(
            Address {
                network_id: 1,
                node_id: upstream_node,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(
        upstream_state(),
        Some(InterfaceState::Active {
            net_id: 1,
            node_id: upstream_node
        }),
    );

    // Quiet link: revert to link-local with the same node_id.
    sleep(Duration::from_millis(900)).await;
    assert_eq!(
        upstream_state(),
        Some(InterfaceState::link_local(upstream_node))
    );
}

// ---- bus_claim sends from the candidate ----

fn spawn_bus_tx(tap: common::BusTap, queue: StdQueue) {
    let queue: &'static StdQueue = Box::leak(Box::new(queue));
    let consumer = queue.framed_consumer();
    tokio::spawn(async move {
        loop {
            let grant = consumer.wait_read().await;
            tap.send(&grant);
            grant.release();
        }
    });
}

/// A claim request must come from the candidate it asks for. Every device
/// hears the response on a shared bus, so a request from the previous
/// candidate (owned by someone else after a conflict) or from a shared boot
/// default would address the response to the wrong device.
#[tokio::test]
async fn claim_retry_sends_each_request_from_its_candidate() {
    let _ = env_logger::builder().is_test(true).try_init();
    let bus = Bus::new();

    // Router on the bus, with node 10 already held by another device.
    let router_queue = new_std_queue(4096);
    let router_stack = BusRouterStack::new_with_profile(Router::new_std());
    let ident = router_stack
        .manage_profile(|im| {
            im.register_interface(framed_stream::Sink::new_from_handle(
                router_queue.clone(),
                250,
            ))
        })
        .unwrap();
    let net_id = router_stack
        .manage_profile(|im| im.net_id_of(ident))
        .unwrap();
    router_stack
        .manage_profile(|im| im.request_node_claim(net_id, 10, 0xAAAA))
        .unwrap();
    spawn_bus_tx(bus.tap(), router_queue);
    tokio::spawn({
        let stack = router_stack.clone();
        let mut tap = bus.tap();
        async move {
            let mut proc = RouterFrameProcessor::new(net_id);
            loop {
                let data = tap.recv().await;
                if data.is_empty() {
                    break;
                }
                proc.process_frame(&data, &stack, ident);
            }
        }
    });
    tokio::spawn({
        let s = router_stack.clone();
        async move { s.services().address_claim_handler::<4>().await }
    });

    // Sniffer: the src node of every claim request (dst port 0) on the bus.
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    tokio::spawn({
        let requests = requests.clone();
        let mut tap = bus.tap();
        async move {
            loop {
                let data = tap.recv().await;
                if data.is_empty() {
                    break;
                }
                if let Some(frame) = de_frame(&data)
                    && frame.hdr.dst.port_id == 0
                {
                    requests.lock().unwrap().push(frame.hdr.src.node_id);
                }
            }
        }
    });

    // Edge boots on a shared default node_id.
    let edge_queue = new_std_queue(4096);
    let edge_stack = bus_edge(&edge_queue);
    set_edge_state(&edge_stack, InterfaceState::edge_link_local());
    spawn_bus_tx(bus.tap(), edge_queue);
    tokio::spawn({
        let stack = edge_stack.clone();
        let mut tap = bus.tap();
        async move {
            let mut proc = EdgeFrameProcessor::new();
            loop {
                let data = tap.recv().await;
                if data.is_empty() {
                    break;
                }
                proc.process_frame(&data, &stack, ());
            }
        }
    });
    sleep(Duration::from_millis(50)).await;

    let lease = timeout(
        Duration::from_secs(5),
        bus_claim_with_retry(&edge_stack, (), [10u8, 11u8], 0xBBBB),
    )
    .await
    .expect("claim timed out")
    .expect("11 is free");

    assert_eq!(lease.node_id, 11);
    assert_eq!(*requests.lock().unwrap(), [10, 11]);
    assert_eq!(
        edge_state(&edge_stack),
        Some(InterfaceState::Active {
            net_id,
            node_id: 11
        })
    );
}

/// A claim that is cancelled before a grant (here: nobody answers and the
/// caller times out) leaves the device on its previous address, not on the
/// candidate it never got.
#[tokio::test]
async fn cancelled_claim_restores_the_previous_state() {
    let edge_stack = bus_edge(&new_std_queue(4096));
    set_edge_state(&edge_stack, InterfaceState::edge_link_local());

    let res = timeout(
        Duration::from_millis(100),
        bus_claim_with_retry(&edge_stack, (), [42u8], 0xCCCC),
    )
    .await;

    assert!(res.is_err(), "nobody answers, so the claim must time out");
    assert_eq!(
        edge_state(&edge_stack),
        Some(InterfaceState::edge_link_local())
    );
}
