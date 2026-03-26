//! Red test: RouterFrameProcessor holds a stale net_id after reassign.
//!
//! When a bridge downstream is registered with `register_interface_pending`,
//! the transport creates `RouterFrameProcessor::new(0)`. After
//! `reassign_interface_net_id(ident, 3)`, the slot has net_id=3 but the
//! already-spawned processor still holds 0. This causes `process_frame` to
//! rewrite `src.network_id` to 0 instead of 3 for frames with src=0.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::sync::{Arc, Mutex};

use ergot::{
    Address, FrameKind, HeaderSeq, ProtocolError,
    interface_manager::{
        FrameProcessor, Interface, InterfaceSink, InterfaceState, Profile,
        profiles::router::{Router, RouterFrameProcessor},
    },
    net_stack::ArcNetStack,
    wire_frames,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;

// --- Sink that captures src.network_id of forwarded frames ---

#[derive(Clone)]
struct CaptureSink {
    frames: Arc<Mutex<Vec<(Address, Address)>>>,
}

impl CaptureSink {
    fn new(frames: Arc<Mutex<Vec<(Address, Address)>>>) -> Self {
        Self { frames }
    }
}

impl InterfaceSink for CaptureSink {
    fn send_ty<T: Serialize>(&mut self, hdr: &HeaderSeq, _body: &T) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
    fn send_raw(&mut self, hdr: &HeaderSeq, _body: &[u8]) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
    fn send_err(&mut self, hdr: &HeaderSeq, _err: ProtocolError) -> Result<(), ()> {
        self.frames.lock().unwrap().push((hdr.src, hdr.dst));
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = CaptureSink;
}

type TestRouter = Router<MockInterface, rand::rngs::StdRng, 8, 8>;
type TestStack = ArcNetStack<CriticalSectionRawMutex, TestRouter>;

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

/// After `register_interface_pending` + `reassign_interface_net_id(ident, 3)`,
/// a RouterFrameProcessor created with net_id=0 rewrites src.network_id to 0
/// instead of the correct 3.
#[test]
fn stale_processor_rewrites_src_to_zero_after_reassign() {
    let frames = Arc::new(Mutex::new(Vec::new()));

    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([0u8; 32])));

    // Register a normal interface: ident=0, net_id=1
    let _dest_ident = stack
        .manage_profile(|im| im.register_interface(CaptureSink::new(frames.clone())))
        .unwrap();

    // Register a pending interface (simulates bridge downstream before seed assign)
    let pending_ident = stack
        .manage_profile(|im| im.register_interface_pending(CaptureSink::new(frames.clone())))
        .unwrap();

    // Transport spawns processor with the only net_id it knows: 0
    let mut processor = RouterFrameProcessor::new(0);

    // Bridge seed assign: reassign pending interface to globally-routable net_id=3
    stack
        .manage_profile(|im| im.reassign_interface_net_id(pending_ident, 3))
        .unwrap();

    // Verify the slot is now Active with net_id=3
    let state = stack.manage_profile(|im| im.interface_state(pending_ident));
    assert!(
        matches!(state, Some(InterfaceState::Active { net_id: 3, .. })),
        "slot should have net_id=3 after reassign, got {:?}",
        state
    );

    // Frame from an unbootstrapped edge: src.network_id=0, node_id=2 (EDGE)
    // Destination: the other interface (net_id=1, node_id=2)
    let frame_data = make_frame(0, 2, 1, 2, 5);

    // Process through the stale processor (net_id=0)
    processor.process_frame(&frame_data, &stack, pending_ident);

    // Check what the destination interface received
    let captured = frames.lock().unwrap();
    assert!(
        !captured.is_empty(),
        "frame should have been forwarded to dest interface"
    );

    let (src, _dst) = &captured[captured.len() - 1];

    // The processor should have rewritten src.network_id to 3 (the reassigned net_id).
    // BUG: the processor still holds net_id=0, so src.network_id is 0.
    assert_eq!(
        src.network_id, 3,
        "src.network_id should be 3 (the reassigned net_id), \
         but the stale RouterFrameProcessor rewrote it to {}",
        src.network_id
    );
}
