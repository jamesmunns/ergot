//! EdgeFrameProcessor net_id (re)discovery guards.
//!
//! On a bridge upstream, the first frame after a liveness reset used to be
//! trusted blindly: a transit frame (root → some downstream segment) could
//! donate its dst net, and the upstream adopted another segment's net_id as
//! its own ("mislatch") — the bridge then emitted frames src-addressed as a
//! device on that segment, misrouting replies (including seed lease refresh
//! responses) until the next lucky re-discovery. Discovery is now guarded by
//! `Profile::is_transit_net` + a node match, and a known net is sticky
//! across `reset()` so transit frames reactivate the interface instead of
//! hijacking its identity.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use ergot::{
    Address, FrameKind, HeaderSeq,
    interface_manager::{
        FrameProcessor, Interface, InterfaceSink, InterfaceState, Profile,
        profiles::{
            direct_edge::{CENTRAL_NODE_ID, DirectEdge, EDGE_NODE_ID, EdgeFrameProcessor},
            router::{Router, UPSTREAM_IDENT},
        },
    },
    net_stack::ArcNetStack,
    wire_frames,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;

// --- Sink that accepts everything ---

#[derive(Clone)]
struct NullSink;

impl InterfaceSink for NullSink {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        Ok(())
    }
    fn send_raw(&mut self, _hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        Ok(())
    }
    fn send_err(&mut self, _hdr: &HeaderSeq, _err: ergot::ProtocolError) -> Result<(), ()> {
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = NullSink;
}

type BridgeRouter = Router<MockInterface, rand::rngs::StdRng, 4, 8>;
type BridgeStack = ArcNetStack<CriticalSectionRawMutex, BridgeRouter>;

type EdgeProfile = DirectEdge<MockInterface>;
type EdgeStack = ArcNetStack<CriticalSectionRawMutex, EdgeProfile>;

/// A frame from the root (net 10, CENTRAL) to `dst`, as seen by an upstream
/// RX worker.
fn frame_to(dst_net: u16, dst_node: u8) -> Vec<u8> {
    let hdr = HeaderSeq {
        src: Address {
            network_id: 10,
            node_id: CENTRAL_NODE_ID,
            port_id: 1,
        },
        dst: Address {
            network_id: dst_net,
            node_id: dst_node,
            port_id: 5,
        },
        any_all: None,
        seq_no: 0,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };
    wire_frames::encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &42u32).unwrap()
}

/// Bridge stack with one downstream slot (net 1) and one seed route (net 2).
/// The upstream's "real" net, as the root would assign it, is 7 in these
/// tests — deliberately absent from the bridge's own tables.
fn bridge_stack() -> BridgeStack {
    let stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge(
        rand::rngs::StdRng::from_seed([0u8; 32]),
        NullSink,
    ));
    let slot_ident = stack
        .manage_profile(|im| im.register_interface(NullSink))
        .unwrap();
    let slot_net = stack.manage_profile(|im| im.net_id_of(slot_ident)).unwrap();
    assert_eq!(slot_net, 1);
    let seed = stack
        .manage_profile(|im| im.request_seed_net_assign(slot_net))
        .unwrap();
    assert_eq!(seed.net_id, 2);
    stack
}

fn upstream_state(stack: &BridgeStack) -> InterfaceState {
    stack
        .manage_profile(|im| im.interface_state(UPSTREAM_IDENT))
        .unwrap()
}

/// `FrameProcessor::reset` is generic over the stack handle, so a bare
/// `proc.reset()` can't infer `N` — qualify it.
fn reset_bridge_proc(p: &mut EdgeFrameProcessor) {
    <EdgeFrameProcessor as FrameProcessor<BridgeStack>>::reset(p);
}

fn reset_edge_proc(p: &mut EdgeFrameProcessor) {
    <EdgeFrameProcessor as FrameProcessor<EdgeStack>>::reset(p);
}

#[test]
fn router_transit_nets_are_slots_and_seed_routes() {
    let stack = bridge_stack();
    stack.manage_profile(|im| {
        assert!(im.is_transit_net(1), "downstream slot net is transit");
        assert!(im.is_transit_net(2), "seed-assigned net is transit");
        assert!(!im.is_transit_net(7), "the upstream's own net is not");
        assert!(!im.is_transit_net(0), "link-local is never transit");
    });
}

#[test]
fn boot_discovery_latches_only_from_bridge_addressed_frames() {
    let stack = bridge_stack();
    // Canonical upstream boot state: link-local placeholder.
    stack
        .manage_profile(|im| {
            im.set_interface_state(
                UPSTREAM_IDENT,
                InterfaceState::Active {
                    net_id: 0,
                    node_id: EDGE_NODE_ID,
                },
            )
        })
        .unwrap();
    let mut proc = EdgeFrameProcessor::new();

    // A transit frame arriving first at boot must NOT latch.
    let changed = proc.process_frame(&frame_to(1, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(!changed);
    assert!(
        matches!(
            upstream_state(&stack),
            InterfaceState::Active { net_id: 0, .. }
        ),
        "transit frame at boot must not donate a net_id"
    );

    // A bridge-addressed frame latches the real upstream net.
    let changed = proc.process_frame(&frame_to(7, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(changed);
    assert!(matches!(
        upstream_state(&stack),
        InterfaceState::Active { net_id: 7, .. }
    ));

    // Steady state: later transit frames change nothing.
    let changed = proc.process_frame(&frame_to(2, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(!changed);
    assert!(matches!(
        upstream_state(&stack),
        InterfaceState::Active { net_id: 7, .. }
    ));
}

/// The mislatch regression: after a liveness reset, a transit frame arriving
/// first must reactivate the upstream with its OLD net, not hijack it.
#[test]
fn transit_frame_after_reset_reactivates_instead_of_hijacking() {
    let stack = bridge_stack();
    stack
        .manage_profile(|im| {
            im.set_interface_state(
                UPSTREAM_IDENT,
                InterfaceState::Active {
                    net_id: 0,
                    node_id: EDGE_NODE_ID,
                },
            )
        })
        .unwrap();
    let mut proc = EdgeFrameProcessor::new();
    proc.process_frame(&frame_to(7, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(matches!(
        upstream_state(&stack),
        InterfaceState::Active { net_id: 7, .. }
    ));

    for transit_net in [1u16, 2] {
        // Liveness timeout: the RX worker marks the interface Inactive and
        // resets the processor.
        stack
            .manage_profile(|im| im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Inactive))
            .unwrap();
        reset_bridge_proc(&mut proc);

        // First frame after the quiet period is transit (root → downstream
        // segment). The old blind latch adopted `transit_net` here.
        let changed =
            proc.process_frame(&frame_to(transit_net, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
        assert!(changed, "reactivation is a state change");
        assert!(
            matches!(
                upstream_state(&stack),
                InterfaceState::Active { net_id: 7, .. }
            ),
            "upstream must keep its own net_id, not adopt transit net {transit_net}"
        );
    }
}

/// Renumbering: after a reset, a bridge-addressed frame with a DIFFERENT
/// (non-transit) net updates the upstream — e.g. the root rebooted and
/// assigned this segment a new net_id.
#[test]
fn rediscovery_accepts_renumbered_upstream_net() {
    let stack = bridge_stack();
    stack
        .manage_profile(|im| {
            im.set_interface_state(
                UPSTREAM_IDENT,
                InterfaceState::Active {
                    net_id: 0,
                    node_id: EDGE_NODE_ID,
                },
            )
        })
        .unwrap();
    let mut proc = EdgeFrameProcessor::new();
    proc.process_frame(&frame_to(7, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);

    stack
        .manage_profile(|im| im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Inactive))
        .unwrap();
    reset_bridge_proc(&mut proc);

    let changed = proc.process_frame(&frame_to(9, EDGE_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(changed);
    assert!(matches!(
        upstream_state(&stack),
        InterfaceState::Active { net_id: 9, .. }
    ));
}

/// Frames addressed to the far side's node (CENTRAL, from an edge's point of
/// view) never donate a net — only frames addressed to us do.
#[test]
fn frames_to_other_nodes_do_not_donate_a_net() {
    let stack = bridge_stack();
    stack
        .manage_profile(|im| im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Inactive))
        .unwrap();
    let mut proc = EdgeFrameProcessor::new();

    let changed = proc.process_frame(&frame_to(7, CENTRAL_NODE_ID), &stack, UPSTREAM_IDENT);
    assert!(!changed);
    assert!(matches!(upstream_state(&stack), InterfaceState::Inactive));
}

/// A controller-mode edge must reactivate as CENTRAL after a liveness reset.
/// The old code cleared the net on reset and re-latched from the next frame
/// with `node_id: EDGE`, silently corrupting the controller's own node.
#[test]
fn controller_reactivates_as_central() {
    let stack: EdgeStack = EdgeStack::new_with_profile(DirectEdge::new_controller(
        NullSink,
        InterfaceState::Active {
            net_id: 5,
            node_id: CENTRAL_NODE_ID,
        },
    ));
    let mut proc = EdgeFrameProcessor::new_controller(5);

    // Liveness timeout on the controller side.
    stack
        .manage_profile(|im| im.set_interface_state((), InterfaceState::Inactive))
        .unwrap();
    reset_edge_proc(&mut proc);

    // Frame from the target (addressed to us, the controller).
    let hdr = HeaderSeq {
        src: Address {
            network_id: 5,
            node_id: EDGE_NODE_ID,
            port_id: 1,
        },
        dst: Address {
            network_id: 5,
            node_id: CENTRAL_NODE_ID,
            port_id: 5,
        },
        any_all: None,
        seq_no: 0,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };
    let frame =
        wire_frames::encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &42u32).unwrap();

    let changed = proc.process_frame(&frame, &stack, ());
    assert!(changed);
    let state = stack.manage_profile(|im| im.interface_state(())).unwrap();
    assert!(
        matches!(
            state,
            InterfaceState::Active {
                net_id: 5,
                node_id: CENTRAL_NODE_ID,
            }
        ),
        "controller must come back as CENTRAL, got {state:?}"
    );
}
