//! The link-layer addressing ([`LinkMeta`]) a port hands its sink, and the
//! broadcast header it goes with.
//!
//! A shared-medium sink (CAN, ESP-NOW, RS-485) addresses frames on the wire
//! from `LinkMeta`, so the next-hop rules live in the port rather than in each
//! sink: a broadcast goes to every node, an on-segment destination directly,
//! and off-segment traffic to the segment router (from a target) or to every
//! node (from the router, which only knows the interface).

#![cfg(feature = "tokio-std")]
// The std bbqueue behind the receiving edge trips Miri's leak checker, as in
// the other std-queue tests.
#![cfg(not(miri))]

use std::sync::{Arc, Mutex};

use ergot::{
    Address, AnyAllAppendix, FrameKind, Header, Key, ProtocolError, TrafficClass,
    interface_manager::{
        FrameProcessor, Interface, InterfaceSink, InterfaceState, LinkDst, LinkMeta, Profile,
        SeedAssignmentError, SeedLease,
        profiles::{
            direct_edge::{
                BROADCAST_NODE_ID, CENTRAL_NODE_ID, DirectEdge, EDGE_NODE_ID, EdgeFrameProcessor,
            },
            router::Router,
        },
        utils::{framed_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    wire_frames::encode_frame_ty,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;

type Log = Arc<Mutex<Vec<(LinkMeta, Address)>>>;

/// Records the `LinkMeta` and header destination of every send.
struct LinkSink {
    log: Log,
}

impl LinkSink {
    fn record(&self, link: &LinkMeta, hdr: &Header) -> Result<(), ()> {
        self.log.lock().unwrap().push((*link, hdr.dst));
        Ok(())
    }
}

impl InterfaceSink for LinkSink {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, link: &LinkMeta, hdr: &Header, _: &T) -> Result<(), ()> {
        self.record(link, hdr)
    }
    fn send_raw(&mut self, link: &LinkMeta, hdr: &Header, _: &[u8]) -> Result<(), ()> {
        self.record(link, hdr)
    }
    fn send_err(&mut self, link: &LinkMeta, hdr: &Header, _: ProtocolError) -> Result<(), ()> {
        self.record(link, hdr)
    }
}

struct LinkInterface;
impl Interface for LinkInterface {
    type Sink = LinkSink;
}

const BUS_NET: u16 = 5;
const OWN_NODE: u8 = 30;

fn header(dst: Address) -> Header {
    let any_all = [0, 255].contains(&dst.port_id);
    Header {
        src: Address {
            network_id: 0,
            node_id: 0,
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
    }
}

fn addr(network_id: u16, node_id: u8, port_id: u8) -> Address {
    Address {
        network_id,
        node_id,
        port_id,
    }
}

/// Send `dst` through `profile` and return the one recorded send.
fn send_one<P: Profile>(profile: &mut P, log: &Log, dst: Address) -> (LinkMeta, Address) {
    log.lock().unwrap().clear();
    profile.send(&header(dst), &0u8).unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1, "expected exactly one send, got {log:?}");
    log[0]
}

fn node(src_node: u8, dst: u8) -> LinkMeta {
    LinkMeta {
        src_node,
        dst: LinkDst::Node(dst),
    }
}

fn broadcast(src_node: u8) -> LinkMeta {
    LinkMeta {
        src_node,
        dst: LinkDst::Broadcast,
    }
}

// ---- Target role (edge / bridge upstream) ----

fn bus_edge(log: &Log) -> DirectEdge<LinkInterface> {
    let mut edge = DirectEdge::new_target(LinkSink { log: log.clone() });
    edge.set_interface_state(
        (),
        InterfaceState::Active {
            net_id: BUS_NET,
            node_id: OWN_NODE,
        },
    )
    .unwrap();
    edge
}

#[test]
fn target_addresses_an_on_segment_destination_directly() {
    let log = Log::default();
    let mut edge = bus_edge(&log);

    let (link, dst) = send_one(&mut edge, &log, addr(BUS_NET, 40, 3));
    assert_eq!(link, node(OWN_NODE, 40));
    assert_eq!(dst, addr(BUS_NET, 40, 3));
}

#[test]
fn target_addresses_a_link_local_destination_directly() {
    let log = Log::default();
    let mut edge = bus_edge(&log);

    let (link, _) = send_one(&mut edge, &log, addr(0, CENTRAL_NODE_ID, 0));
    assert_eq!(link, node(OWN_NODE, CENTRAL_NODE_ID));
}

#[test]
fn target_sends_off_segment_traffic_to_the_segment_router() {
    let log = Log::default();
    let mut edge = bus_edge(&log);

    let (link, dst) = send_one(&mut edge, &log, addr(9, 7, 3));
    assert_eq!(link, node(OWN_NODE, CENTRAL_NODE_ID));
    assert_eq!(dst, addr(9, 7, 3), "the header keeps the end destination");
}

#[test]
fn target_broadcast_is_addressed_to_every_node() {
    let log = Log::default();
    let mut edge = bus_edge(&log);

    let (link, dst) = send_one(&mut edge, &log, addr(0, 0, 255));
    assert_eq!(link, broadcast(OWN_NODE));
    assert_eq!(dst, addr(BUS_NET, BROADCAST_NODE_ID, 255));
}

// ---- Controller role (router downstream slot) ----

type TestRouter = Router<LinkInterface, rand::rngs::StdRng, 4, 4>;

fn router(log: &Log) -> (TestRouter, u16) {
    let mut router = TestRouter::new(rand::rngs::StdRng::seed_from_u64(0));
    let ident = router
        .register_interface(LinkSink { log: log.clone() })
        .unwrap();
    let net_id = router.net_id_of(ident).unwrap();
    (router, net_id)
}

#[test]
fn router_addresses_a_segment_node_directly() {
    let log = Log::default();
    let (mut router, net_id) = router(&log);

    let (link, _) = send_one(&mut router, &log, addr(net_id, 40, 3));
    assert_eq!(link, node(CENTRAL_NODE_ID, 40));
}

#[test]
fn router_broadcast_is_addressed_to_every_node() {
    let log = Log::default();
    let (mut router, net_id) = router(&log);

    let (link, dst) = send_one(&mut router, &log, addr(0, 0, 255));
    assert_eq!(link, broadcast(CENTRAL_NODE_ID));
    assert_eq!(dst, addr(net_id, BROADCAST_NODE_ID, 255));
}

/// A net behind a device on this segment is reached through that device —
/// the node that requested its seed lease — not by broadcasting to every node
/// on the segment.
#[test]
fn router_forwards_a_seeded_net_through_its_requester() {
    let log = Log::default();
    let (mut router, net_id) = router(&log);
    let behind = router.request_seed_net_assign(net_id, 30).unwrap().net_id;

    let (link, dst) = send_one(&mut router, &log, addr(behind, EDGE_NODE_ID, 3));
    assert_eq!(link, node(CENTRAL_NODE_ID, 30));
    assert_eq!(
        dst,
        addr(behind, EDGE_NODE_ID, 3),
        "the header keeps the end destination"
    );
}

/// A refresh proves the requester holds the lease, so it also moves the next
/// hop: a bridge that re-claimed a different node_id repairs its route. A
/// refresh with a bad token moves nothing.
#[test]
fn a_seed_refresh_moves_the_next_hop() {
    let log = Log::default();
    let (mut router, net_id) = router(&log);
    let lease = router.request_seed_net_assign(net_id, 30).unwrap();
    let behind = addr(lease.net_id, EDGE_NODE_ID, 3);

    assert!(
        router
            .refresh_seed_net_assignment(net_id, 40, lease.net_id, [0xFF; 8])
            .is_err()
    );
    assert_eq!(
        send_one(&mut router, &log, behind).0,
        node(CENTRAL_NODE_ID, 30)
    );

    // A fresh lease (30 s) is already inside the refresh window (< 62 s left).
    router
        .refresh_seed_net_assignment(net_id, 40, lease.net_id, lease.refresh_token)
        .unwrap();
    assert_eq!(
        send_one(&mut router, &log, behind).0,
        node(CENTRAL_NODE_ID, 40)
    );
}

/// A bridge on this router's segment reaches the net it delegated for its
/// own requester through that requester, and follows it on refresh.
#[test]
fn a_delegated_route_goes_through_its_requester() {
    let log = Log::default();
    let mut bridge = TestRouter::new_bridge(
        rand::rngs::StdRng::seed_from_u64(1),
        LinkSink { log: log.clone() },
    );
    let ident = bridge
        .register_interface_pending(LinkSink { log: log.clone() })
        .unwrap();
    bridge.reassign_interface_net_id(ident, 20).unwrap();

    let parent = |expires| SeedLease {
        net_id: 21,
        refresh_addr: addr(1, CENTRAL_NODE_ID, 42),
        release_addr: addr(1, CENTRAL_NODE_ID, 43),
        refresh_token: [7; 8],
        expires_seconds: expires,
        max_refresh_seconds: 120,
        min_refresh_seconds: 62,
    };
    let lease = bridge
        .register_delegated_seed_net(20, 30, &parent(30))
        .unwrap();
    let behind = addr(21, EDGE_NODE_ID, 3);
    assert_eq!(
        send_one(&mut bridge, &log, behind).0,
        node(CENTRAL_NODE_ID, 30)
    );

    bridge
        .commit_delegated_refresh(20, 40, lease.refresh_token, &parent(120))
        .unwrap();
    assert_eq!(
        send_one(&mut bridge, &log, behind).0,
        node(CENTRAL_NODE_ID, 40)
    );
}

/// "This node" (0) and the broadcast address cannot be a next hop.
#[test]
fn a_seed_request_from_a_non_node_is_refused() {
    let log = Log::default();
    let (mut router, net_id) = router(&log);
    for node in [0, BROADCAST_NODE_ID] {
        assert_eq!(
            router.request_seed_net_assign(net_id, node),
            Err(SeedAssignmentError::UnknownSource)
        );
    }
}

// ---- Receiving a broadcast ----

type EdgeStack = ArcNetStack<
    CriticalSectionRawMutex,
    DirectEdge<ergot::interface_manager::interface_impls::tokio_udp::TokioUdpInterface>,
>;

fn edge_stack() -> EdgeStack {
    EdgeStack::new_with_profile(DirectEdge::new_target(
        framed_stream::Sink::new_from_handle(new_std_queue(4096), 512),
    ))
}

fn broadcast_frame(net_id: u16) -> Vec<u8> {
    let mut hdr = header(addr(net_id, BROADCAST_NODE_ID, 255));
    hdr.src = addr(net_id, CENTRAL_NODE_ID, 10);
    encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &0u8).unwrap()
}

/// A point-to-point edge still learns its net from the router's broadcasts,
/// now that they are addressed to node 255 instead of `EDGE_NODE_ID`.
#[test]
fn point_to_point_edge_learns_its_net_from_a_broadcast() {
    let stack = edge_stack();
    stack
        .manage_profile(|im| im.set_interface_state((), InterfaceState::edge_link_local()))
        .unwrap();

    let changed = EdgeFrameProcessor::new().process_frame(&broadcast_frame(7), &stack, ());
    assert!(changed);
    assert_eq!(
        stack.manage_profile(|im| im.interface_state(())),
        Some(InterfaceState::Active {
            net_id: 7,
            node_id: EDGE_NODE_ID
        })
    );
}

/// A bus edge learns the segment's net from a broadcast and keeps its node.
#[test]
fn bus_edge_learns_its_net_from_a_broadcast_and_keeps_its_node() {
    let stack = edge_stack();
    stack
        .manage_profile(|im| im.set_interface_state((), InterfaceState::link_local(OWN_NODE)))
        .unwrap();

    EdgeFrameProcessor::new().process_frame(&broadcast_frame(BUS_NET), &stack, ());
    assert_eq!(
        stack.manage_profile(|im| im.interface_state(())),
        Some(InterfaceState::Active {
            net_id: BUS_NET,
            node_id: OWN_NODE
        })
    );
}
