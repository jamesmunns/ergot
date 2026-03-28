//! Tests for FrameProcessor implementations (EdgeFrameProcessor, RouterFrameProcessor).

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use ergot::{
    Address, FrameKind, HeaderSeq,
    interface_manager::{
        FrameProcessor, InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::{DirectEdge, EdgeFrameProcessor},
        profiles::router::{Router, RouterFrameProcessor},
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    wire_frames,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;
type RouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

/// Build a valid ergot frame (CommonHeader + postcard body).
fn make_frame(src_net: u16, src_node: u8, dst_net: u16, dst_node: u8, dst_port: u8) -> Vec<u8> {
    let hdr = HeaderSeq {
        src: Address {
            network_id: src_net,
            node_id: src_node,
            port_id: 1,
        },
        dst: Address {
            network_id: dst_net,
            node_id: dst_node,
            port_id: dst_port,
        },
        any_all: None,
        seq_no: 0,
        kind: FrameKind::ENDPOINT_REQ,
        ttl: 16,
    };
    wire_frames::encode_frame_ty(postcard::ser_flavors::StdVec::new(), &hdr, &42u32).unwrap()
}

fn make_edge_stack() -> EdgeStack {
    let queue = new_std_queue(4096);
    EdgeStack::new_with_profile(DirectEdge::new_target(cobs_stream::Sink::new_from_handle(
        queue, 512,
    )))
}

// ---------------------------------------------------------------------------
// EdgeFrameProcessor
// ---------------------------------------------------------------------------

#[test]
fn edge_discovers_net_id_returns_true() {
    let stack = make_edge_stack();
    // Set interface to Inactive so process_frame can transition to Active
    stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = EdgeFrameProcessor::new();
    // Frame with dst net_id=5, node_id=2 (EDGE_NODE_ID)
    let frame = make_frame(5, 1, 5, 2, 10);
    let changed = proc.process_frame(&frame, &stack, ());
    assert!(changed, "should detect net_id change on first frame");
}

#[test]
fn edge_same_net_id_returns_false() {
    let stack = make_edge_stack();
    stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = EdgeFrameProcessor::new();
    let frame = make_frame(5, 1, 5, 2, 10);

    proc.process_frame(&frame, &stack, ());
    let changed = proc.process_frame(&frame, &stack, ());
    assert!(!changed, "same net_id should not be detected as change");
}

#[test]
fn edge_reset_allows_rediscovery() {
    let stack = make_edge_stack();
    stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = EdgeFrameProcessor::new();
    let frame = make_frame(5, 1, 5, 2, 10);

    proc.process_frame(&frame, &stack, ());
    <EdgeFrameProcessor as FrameProcessor<EdgeStack>>::reset(&mut proc);

    // After reset, need to set Inactive again for process_frame to transition
    stack.manage_profile(|im| {
        im.set_interface_state((), InterfaceState::Inactive)
            .unwrap();
    });

    let changed = proc.process_frame(&frame, &stack, ());
    assert!(changed, "should rediscover net_id after reset");
}

#[test]
fn edge_controller_has_preset_net_id() {
    let queue = new_std_queue(4096);
    let stack: EdgeStack = EdgeStack::new_with_profile(DirectEdge::new_controller(
        cobs_stream::Sink::new_from_handle(queue, 512),
        InterfaceState::Active {
            net_id: 1,
            node_id: 1,
        },
    ));

    let mut proc = EdgeFrameProcessor::new_controller(1);
    // Frame from net_id=1 — should NOT be a change since controller already knows net_id=1
    let frame = make_frame(1, 2, 1, 1, 10);
    let changed = proc.process_frame(&frame, &stack, ());
    // Controller has preset net_id, so process_frame should set it and not detect a change
    // (net_id was already Some(1) from new_controller, frame also sets it to 1)
    assert!(
        !changed,
        "controller with matching net_id should not detect change"
    );
}

// ---------------------------------------------------------------------------
// RouterFrameProcessor (Router)
// ---------------------------------------------------------------------------

#[test]
fn router_processor_activates_on_first_frame() {
    let stack: RouterStack = RouterStack::new();

    // Register an interface — starts as Active
    let ident = stack.manage_profile(|im| {
        let q = new_std_queue(4096);
        im.register_interface(cobs_stream::Sink::new_from_handle(q, 512))
    });
    let ident = ident.unwrap();
    let net_id = match stack.manage_profile(|im| im.interface_state(ident)) {
        Some(InterfaceState::Active { net_id, .. }) => net_id,
        other => panic!("expected Active, got {:?}", other),
    };

    // Set to Inactive (simulating liveness timeout)
    stack.manage_profile(|im| {
        im.set_interface_state(ident, InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = RouterFrameProcessor::new(net_id);
    // Frame from edge device (net_id, node=2)
    let frame = make_frame(net_id, 2, 0, 0, 10);
    let changed = proc.process_frame(&frame, &stack, ident);
    assert!(changed, "first frame should activate interface");

    // Verify state is now Active
    let state = stack.manage_profile(|im| im.interface_state(ident));
    assert!(
        matches!(state, Some(InterfaceState::Active { .. })),
        "expected Active after first frame, got {:?}",
        state
    );
}

#[test]
fn router_processor_returns_false_after_activation() {
    let stack: RouterStack = RouterStack::new();
    let ident = stack
        .manage_profile(|im| {
            let q = new_std_queue(4096);
            im.register_interface(cobs_stream::Sink::new_from_handle(q, 512))
        })
        .unwrap();
    let net_id = match stack.manage_profile(|im| im.interface_state(ident)) {
        Some(InterfaceState::Active { net_id, .. }) => net_id,
        other => panic!("expected Active, got {:?}", other),
    };

    stack.manage_profile(|im| {
        im.set_interface_state(ident, InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = RouterFrameProcessor::new(net_id);
    let frame = make_frame(net_id, 2, 0, 0, 10);

    proc.process_frame(&frame, &stack, ident); // activates
    let changed = proc.process_frame(&frame, &stack, ident);
    assert!(!changed, "second frame should not trigger activation");
}

#[test]
fn router_processor_reset_allows_reactivation() {
    let stack: RouterStack = RouterStack::new();
    let ident = stack
        .manage_profile(|im| {
            let q = new_std_queue(4096);
            im.register_interface(cobs_stream::Sink::new_from_handle(q, 512))
        })
        .unwrap();
    let net_id = match stack.manage_profile(|im| im.interface_state(ident)) {
        Some(InterfaceState::Active { net_id, .. }) => net_id,
        other => panic!("expected Active, got {:?}", other),
    };

    stack.manage_profile(|im| {
        im.set_interface_state(ident, InterfaceState::Inactive)
            .unwrap();
    });

    let mut proc = RouterFrameProcessor::new(net_id);
    let frame = make_frame(net_id, 2, 0, 0, 10);

    proc.process_frame(&frame, &stack, ident); // activates
    <RouterFrameProcessor as FrameProcessor<RouterStack>>::reset(&mut proc);

    // Set Inactive again
    stack.manage_profile(|im| {
        im.set_interface_state(ident, InterfaceState::Inactive)
            .unwrap();
    });

    let changed = proc.process_frame(&frame, &stack, ident);
    assert!(changed, "should reactivate after reset");
}
