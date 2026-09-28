//! Static node_ids: a device with a fixed address on a bus segment, reserved
//! with the router instead of claimed through the address claim protocol.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::{
    pin::pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use common::Bus;
use ergot::{
    Address, FrameKind, Header, ProtocolError,
    interface_manager::{
        AddressClaimError, FrameProcessor, Interface, InterfaceSink, InterfaceState, LinkMeta,
        Profile,
        profiles::{
            direct_edge::{DirectEdge, EdgeFrameProcessor},
            router::{Router, RouterFrameProcessor, StaticNodeError},
        },
        utils::{
            framed_stream,
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::ArcNetStack,
    well_known::ErgotPingEndpoint,
    wire_frames,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;
use tokio::time::{sleep, timeout};

// ---- The router alone ----

/// Records the (src, dst) of every frame the router sends through it.
#[derive(Clone, Default)]
struct CaptureSink {
    frames: Arc<Mutex<Vec<(Address, Address)>>>,
}

impl InterfaceSink for CaptureSink {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _: &LinkMeta, hdr: &Header, _: &T) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
    fn send_raw(&mut self, _: &LinkMeta, hdr: &Header, _: &[u8]) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
    fn send_err(&mut self, _: &LinkMeta, hdr: &Header, _: ProtocolError) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = CaptureSink;
}

/// No claim capacity at all: fixed addresses only.
type StaticRouter = Router<MockInterface, rand::rngs::StdRng, 4, 4, 0>;

fn router<const C: usize>() -> Router<MockInterface, rand::rngs::StdRng, 4, 4, C> {
    Router::new(rand::rngs::StdRng::from_seed([7; 32]))
}

/// A frame from `src_node` on `src_net` to a specific port on `dst_net`.
fn frame(src_net: u16, src_node: u8, dst_net: u16) -> Vec<u8> {
    let hdr = Header {
        src: Address {
            network_id: src_net,
            node_id: src_node,
            port_id: 1,
        },
        dst: Address {
            network_id: dst_net,
            node_id: 2,
            port_id: 5,
        },
        any_all: None,
        kind: FrameKind::ENDPOINT_REQ,
        class: ergot::TrafficClass::Normal,
        ttl: 15,
    };
    wire_frames::encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &42u32).unwrap()
}

#[test]
fn a_static_node_sends_on_a_router_without_claim_capacity() {
    let forwarded = CaptureSink::default();
    let stack = ArcNetStack::<CriticalSectionRawMutex, StaticRouter>::new_with_profile(router());
    let bus = stack
        .manage_profile(|im| im.register_interface(CaptureSink::default()))
        .unwrap();
    let other = stack
        .manage_profile(|im| im.register_interface(forwarded.clone()))
        .unwrap();
    let (bus_net, other_net) =
        stack.manage_profile(|im| (im.net_id_of(bus).unwrap(), im.net_id_of(other).unwrap()));
    let mut processor = RouterFrameProcessor::new(bus_net);

    // Not reserved: dropped as unclaimed.
    processor.process_frame(&frame(bus_net, 17, other_net), &stack, bus);
    assert!(forwarded.frames.lock().unwrap().is_empty());

    stack
        .manage_profile(|im| im.reserve_static_node(bus, 17))
        .unwrap();
    processor.process_frame(&frame(bus_net, 17, other_net), &stack, bus);
    // Link-local source: the router fills in the segment's net_id.
    processor.process_frame(&frame(0, 17, other_net), &stack, bus);
    let sent = forwarded.frames.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter()
            .all(|(src, _)| src.network_id == bus_net && src.node_id == 17)
    );
}

#[test]
fn reservations_and_claims_exclude_each_other() {
    let mut r = router::<4>();
    let bus = r.register_interface(CaptureSink::default()).unwrap();
    let net = r.net_id_of(bus).unwrap();

    r.reserve_static_node(bus, 17).unwrap();
    assert_eq!(
        r.request_node_claim(net, 17, 0xAAAA).err(),
        Some(AddressClaimError::Conflict)
    );

    r.request_node_claim(net, 30, 0xBBBB).unwrap();
    assert_eq!(
        r.reserve_static_node(bus, 30),
        Err(StaticNodeError::AlreadyClaimed)
    );

    // Released, the node_id is claimable again.
    assert!(r.release_static_node(bus, 17));
    assert!(!r.is_node_claimed(net, 17));
    r.request_node_claim(net, 17, 0xAAAA).unwrap();
}

#[test]
fn reservation_arguments_are_checked() {
    let mut r = router::<0>();
    let bus = r.register_interface(CaptureSink::default()).unwrap();

    for invalid in [0, 1, 2, 255] {
        assert_eq!(
            r.reserve_static_node(bus, invalid),
            Err(StaticNodeError::InvalidNodeId)
        );
    }
    assert_eq!(
        r.reserve_static_node(bus + 1, 17),
        Err(StaticNodeError::InterfaceNotFound)
    );

    // Reserving twice is fine; releasing twice is not.
    r.reserve_static_node(bus, 17).unwrap();
    r.reserve_static_node(bus, 17).unwrap();
    assert!(r.release_static_node(bus, 17));
    assert!(!r.release_static_node(bus, 17));
    assert!(!r.release_static_node(bus + 1, 17));
}

#[test]
fn a_reservation_is_scoped_to_its_segment() {
    let mut r = router::<0>();
    let a = r.register_interface(CaptureSink::default()).unwrap();
    let b = r.register_interface(CaptureSink::default()).unwrap();
    let (net_a, net_b) = (r.net_id_of(a).unwrap(), r.net_id_of(b).unwrap());

    r.reserve_static_node(a, 17).unwrap();
    assert!(r.is_node_claimed(net_a, 17));
    assert!(!r.is_node_claimed(net_b, 17));
    // Other nodes of the same 32-bit word are not affected.
    assert!(!r.is_node_claimed(net_a, 16));
    assert!(!r.is_node_claimed(net_a, 18));
}

/// A reservation belongs to the segment: it follows the interface to a new
/// net_id (claims do not, see `bus_claim_validation`), and it can be made
/// before the interface has a net_id at all.
#[test]
fn a_reservation_follows_its_interface_through_net_id_assignment() {
    let mut r = router::<0>();
    let bus = r
        .register_interface_pending(CaptureSink::default())
        .unwrap();

    r.reserve_static_node(bus, 17).unwrap();
    // net_id 0 is the pending placeholder, never a segment.
    assert!(!r.is_node_claimed(0, 17));

    r.reassign_interface_net_id(bus, 40).unwrap();
    assert!(r.is_node_claimed(40, 17));

    r.reassign_interface_net_id(bus, 41).unwrap();
    assert!(r.is_node_claimed(41, 17));
    assert!(!r.is_node_claimed(40, 17));
}

#[test]
fn a_reservation_goes_away_with_its_interface() {
    let mut r = router::<0>();
    let bus = r.register_interface(CaptureSink::default()).unwrap();
    let net = r.net_id_of(bus).unwrap();
    r.reserve_static_node(bus, 17).unwrap();

    r.deregister_interface(bus).unwrap();
    assert!(!r.is_node_claimed(net, 17));

    // The same ident and net_id, handed to a new interface, start clean.
    let again = r.register_interface(CaptureSink::default()).unwrap();
    assert_eq!((again, r.net_id_of(again)), (bus, Some(net)));
    assert!(!r.is_node_claimed(net, 17));
}

// ---- On a bus ----

struct BusInterface;
impl Interface for BusInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}

type BusRouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<BusInterface, rand::rngs::StdRng, 4, 4, 0>>;
type BusEdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<BusInterface>>;

const MTU: u16 = 250;

/// Forward everything queued on `queue` onto the bus.
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

/// A device with the fixed node_id 17, not told its segment's net_id, sits on
/// a bus whose router has no claim capacity. The router reaches it once the
/// node_id is reserved — the device learns the net_id from the first frame
/// addressed to it — and not before: its replies are dropped as unclaimed.
#[tokio::test]
async fn a_fixed_address_device_is_reachable_once_reserved() {
    let _ = env_logger::builder().is_test(true).try_init();
    let bus = Bus::new();

    let router_queue = new_std_queue(4096);
    let router: BusRouterStack = BusRouterStack::new_with_profile(Router::new_std());
    let ident = router
        .manage_profile(|im| {
            im.register_interface(framed_stream::Sink::new_from_handle(
                router_queue.clone(),
                MTU,
            ))
        })
        .unwrap();
    let net = router.manage_profile(|im| im.net_id_of(ident)).unwrap();
    spawn_bus_tx(bus.tap(), router_queue);
    tokio::spawn({
        let (stack, mut tap) = (router.clone(), bus.tap());
        async move {
            let mut processor = RouterFrameProcessor::new(net);
            loop {
                processor.process_frame(&tap.recv().await, &stack, ident);
            }
        }
    });

    let edge_queue = new_std_queue(4096);
    let edge: BusEdgeStack = BusEdgeStack::new_with_profile(DirectEdge::new_target(
        framed_stream::Sink::new_from_handle(edge_queue.clone(), MTU),
    ));
    edge.manage_profile(|im| {
        im.set_interface_state(
            (),
            InterfaceState::Active {
                net_id: 0,
                node_id: 17,
            },
        )
    })
    .unwrap();
    spawn_bus_tx(bus.tap(), edge_queue);
    tokio::spawn({
        let (stack, mut tap) = (edge.clone(), bus.tap());
        async move {
            let mut processor = EdgeFrameProcessor::new();
            loop {
                processor.process_frame(&tap.recv().await, &stack, ());
            }
        }
    });
    tokio::spawn({
        let stack = edge.clone();
        async move {
            let server = pin!(
                stack
                    .endpoints()
                    .bounded_server::<ErgotPingEndpoint, 4>(Some("ping"))
            );
            let mut hdl = server.attach();
            loop {
                let _ = hdl.serve(|v: &u32| core::future::ready(*v)).await;
            }
        }
    });
    sleep(Duration::from_millis(50)).await;

    let device = Address {
        network_id: net,
        node_id: 17,
        port_id: 0,
    };
    let unreserved = timeout(
        Duration::from_millis(500),
        router
            .endpoints()
            .request::<ErgotPingEndpoint>(device, &1, Some("ping")),
    )
    .await;
    assert!(
        !matches!(unreserved, Ok(Ok(_))),
        "the reply from an unreserved node_id must be dropped"
    );

    router
        .manage_profile(|im| im.reserve_static_node(ident, 17))
        .unwrap();
    assert_eq!(common::ping_with_retry(&router, device, 2).await, 2);
    assert_eq!(
        edge.manage_profile(|im| im.interface_state(())),
        Some(InterfaceState::Active {
            net_id: net,
            node_id: 17
        })
    );
}
