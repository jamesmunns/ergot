//! process_frame drops frames from unclaimed node_ids on a bus.
//!
//! The router validates the source node_id of every received frame (except
//! port-0 wildcard frames, which may be claim requests) against its claim
//! table. A frame from a node_id that hasn't claimed an address on that
//! segment is dropped; once the node_id is claimed, the same frame routes
//! normally.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::sync::{Arc, Mutex};

use ergot::{
    Address, FrameKind, HeaderSeq, ProtocolError,
    interface_manager::{
        FrameProcessor, Interface, InterfaceSink, Profile,
        profiles::router::{Router, RouterFrameProcessor},
    },
    net_stack::ArcNetStack,
    wire_frames,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;

// --- Sink that captures forwarded frames ---

#[derive(Clone)]
struct CaptureSink {
    frames: Arc<Mutex<Vec<(Address, Address)>>>,
}

impl InterfaceSink for CaptureSink {
    fn mtu(&self) -> u16 {
        2048
    }
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

// C=4: a router with bus claim slots.
type TestRouter = Router<MockInterface, rand::rngs::StdRng, 8, 8, 4>;
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

#[test]
fn unclaimed_node_frame_is_dropped_then_routed_after_claim() {
    let forwarded = Arc::new(Mutex::new(Vec::new()));

    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([0u8; 32])));

    // Interface A = the bus segment (net_id=1) frames arrive on.
    let bus_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: forwarded.clone(),
            })
        })
        .unwrap();
    // Interface B = a downstream (net_id=2) we can observe forwards to.
    let _dest_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: forwarded.clone(),
            })
        })
        .unwrap();

    let mut processor = RouterFrameProcessor::new(1);

    // A frame from node_id=50 on the bus (net_id=1) to a specific (non-zero)
    // port on the downstream. node_id=50 has NOT claimed an address.
    let frame = make_frame(1, 50, 2, 2, 5);

    processor.process_frame(&frame, &stack, bus_ident);
    assert!(
        forwarded.lock().unwrap().is_empty(),
        "frame from an unclaimed node_id must be dropped, not forwarded"
    );

    // Now node_id=50 claims an address on net_id=1.
    let granted = stack
        .manage_profile(|im| im.request_node_claim(1, 50, 0xABCD))
        .unwrap();
    assert_eq!(granted.node_id, 50);

    // The same frame now routes through to the downstream interface.
    processor.process_frame(&frame, &stack, bus_ident);
    let captured = forwarded.lock().unwrap();
    assert_eq!(
        captured.len(),
        1,
        "after claiming, the frame from node_id=50 should be forwarded"
    );
    assert_eq!(
        captured[0].1.network_id, 2,
        "forwarded to the net_id=2 downstream"
    );
}

#[test]
fn transit_frame_from_foreign_net_is_not_dropped() {
    // Claim validation must apply only to frames *originating* on the arrival
    // segment, not to transit frames passing through. A bus device's frame
    // (src net_id=7, claimed node_id=47) routed through a second router arrives
    // on a different interface (net_id=1); node_id=47 is — correctly — not in
    // *this* router's claim table. It must still be forwarded, or multi-hop
    // routing of bus-originated traffic breaks.
    let forwarded = Arc::new(Mutex::new(Vec::new()));

    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([0u8; 32])));

    // Interface A (net_id=1) = the link a transit frame arrives on.
    let arrival_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: forwarded.clone(),
            })
        })
        .unwrap();
    // Interface B (net_id=2) = the downstream we forward toward.
    let _dest_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: forwarded.clone(),
            })
        })
        .unwrap();

    let mut processor = RouterFrameProcessor::new(1);

    // Transit frame: originated on net_id=7 from node_id=47 (claimed on *that*
    // bus), passing through toward net_id=2.
    let frame = make_frame(7, 47, 2, 2, 5);
    processor.process_frame(&frame, &stack, arrival_ident);

    let captured = forwarded.lock().unwrap();
    assert_eq!(
        captured.len(),
        1,
        "a transit frame from a foreign net_id must be forwarded, not dropped"
    );
    assert_eq!(captured[0].1.network_id, 2);
}

#[test]
fn claims_are_purged_when_interface_deregistered() {
    // When a bus interface is torn down, its node_id claims must be dropped:
    // otherwise they linger (until lease expiry) and, if the net_id is reused
    // by a new interface, validate frames or block re-claims on the new bus.
    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([1u8; 32])));

    let bus_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: Arc::new(Mutex::new(Vec::new())),
            })
        })
        .unwrap();
    let bus_net = stack.manage_profile(|im| im.net_id_of(bus_ident)).unwrap();

    stack
        .manage_profile(|im| im.request_node_claim(bus_net, 50, 0xAAAA))
        .unwrap();
    assert!(
        stack.manage_profile(|im| im.is_node_claimed(bus_net, 50)),
        "claim should be active before deregister"
    );

    stack
        .manage_profile(|im| im.deregister_interface(bus_ident))
        .unwrap();

    assert!(
        !stack.manage_profile(|im| im.is_node_claimed(bus_net, 50)),
        "claim must be purged when its interface is deregistered"
    );
}

/// A node-claim refresh whose response is lost must be recoverable: the device
/// retries with the only token it has (the previous one), and the router must
/// accept that as an idempotent replay rather than rejecting it — otherwise the
/// device can never refresh, its lease expires, and it is locked off the bus.
#[test]
fn node_claim_refresh_survives_a_lost_response() {
    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([2u8; 32])));

    let bus_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: Arc::new(Mutex::new(Vec::new())),
            })
        })
        .unwrap();
    let bus_net = stack.manage_profile(|im| im.net_id_of(bus_ident)).unwrap();

    // Device claims node_id 50 and gets its first refresh token.
    let claim = stack
        .manage_profile(|im| im.request_node_claim(bus_net, 50, 0xAAAA))
        .unwrap();
    let token = claim.refresh_token;

    // Device refreshes: the router rotates the token and replies — but the reply
    // is LOST, so the device never learns the new token.
    stack
        .manage_profile(|im| im.refresh_node_claim(bus_net, 50, token))
        .expect("first refresh should succeed");

    // Device retries with the only token it still has.
    let retry = stack.manage_profile(|im| im.refresh_node_claim(bus_net, 50, token));
    assert!(
        retry.is_ok(),
        "a refresh retry with the previous token must be accepted as a replay, got {:?}",
        retry.err()
    );
    assert!(
        stack.manage_profile(|im| im.is_node_claimed(bus_net, 50)),
        "the claim must remain active after a recovered refresh"
    );
}

/// A replayed (idempotent) refresh does not extend the lease, so it must report
/// the lease's actual remaining time — not a fresh full lease — or the client
/// would schedule its next refresh too late and let the claim expire.
#[test]
fn node_claim_replay_reports_remaining_lease() {
    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([5u8; 32])));

    let bus_ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: Arc::new(Mutex::new(Vec::new())),
            })
        })
        .unwrap();
    let bus_net = stack.manage_profile(|im| im.net_id_of(bus_ident)).unwrap();

    let claim = stack
        .manage_profile(|im| im.request_node_claim(bus_net, 50, 0xAAAA))
        .unwrap();
    let token = claim.refresh_token;

    // A real refresh extends the lease and rotates the token; its reply is "lost".
    let refreshed = stack
        .manage_profile(|im| im.refresh_node_claim(bus_net, 50, token))
        .expect("first refresh should succeed");
    let max_lease = refreshed.expires_seconds; // the full lease length (MAX_LEASE_SECS)

    // Let time pass, then retry with the previous token (a replay).
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let replay = stack
        .manage_profile(|im| im.refresh_node_claim(bus_net, 50, token))
        .expect("replay should be accepted");

    assert!(
        replay.expires_seconds < max_lease,
        "a replay must report the reduced remaining lease ({} elapsed since the \
         extend), not the full {}",
        max_lease - replay.expires_seconds,
        max_lease
    );
}

/// Reassigning an interface's net_id must purge node claims scoped to the old
/// net_id, so they don't linger and validate foreign frames (or block re-claims)
/// if that net_id is later reused by another interface.
#[test]
fn node_claims_are_purged_on_net_id_reassignment() {
    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([3u8; 32])));

    let ident = stack
        .manage_profile(|im| {
            im.register_interface(CaptureSink {
                frames: Arc::new(Mutex::new(Vec::new())),
            })
        })
        .unwrap();
    let old_net = stack.manage_profile(|im| im.net_id_of(ident)).unwrap();

    stack
        .manage_profile(|im| im.request_node_claim(old_net, 50, 0xBBBB))
        .unwrap();
    assert!(stack.manage_profile(|im| im.is_node_claimed(old_net, 50)));

    // Reassign the interface to a different, unused net_id.
    let new_net = old_net + 100;
    stack
        .manage_profile(|im| im.reassign_interface_net_id(ident, new_net))
        .unwrap();

    assert!(
        !stack.manage_profile(|im| im.is_node_claimed(old_net, 50)),
        "the old net_id's claim must be purged after reassignment"
    );
}

/// A claim request with source net_id 0 must be rejected. net_id 0 is the pending
/// placeholder; a request arriving on a not-yet-assigned slot must not be able to
/// match that pending slot and be granted a claim scoped to net 0.
#[test]
fn node_claim_with_source_net_zero_is_rejected() {
    let stack: TestStack =
        TestStack::new_with_profile(Router::new(rand::rngs::StdRng::from_seed([4u8; 32])));

    // A pending interface has net_id 0.
    stack
        .manage_profile(|im| {
            im.register_interface_pending(CaptureSink {
                frames: Arc::new(Mutex::new(Vec::new())),
            })
        })
        .unwrap();

    let res = stack.manage_profile(|im| im.request_node_claim(0, 50, 0xCCCC));
    assert!(
        res.is_err(),
        "a claim scoped to net 0 (the pending placeholder) must be rejected, got {res:?}"
    );
}
