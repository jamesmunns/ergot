//! Unit tests for hierarchical seed delegation.
//!
//! These exercise the `Router` delegation hooks directly (the part ported onto
//! `LeaseTable` when this branch was rebased onto bus-address-claim):
//! `seed_delegation_upstream`, `register_delegated_seed_net`,
//! `prepare_delegated_refresh`, and `commit_delegated_refresh`.
//!
//! The end-to-end cascade — a bridge running `seed_router_request_handler`
//! delegating a downstream's request up to the root, and the "no net_id
//! collision across nested bridges" property that motivated this change — is
//! covered in `e2e_seed_delegation.rs`.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use ergot::{
    Address, HeaderSeq, ProtocolError,
    interface_manager::{
        DelegatedRefreshPreparation, Interface, InterfaceSink, Profile, SeedAssignmentError,
        SeedLease, SeedRefreshError,
        profiles::router::{Router, UPSTREAM_IDENT},
    },
    net_stack::ArcNetStack,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;

// A sink that drops everything — these tests inspect return values and the
// claim/route tables, not forwarded frames.
#[derive(Clone)]
struct NullSink;
impl InterfaceSink for NullSink {
    fn mtu(&self) -> u16 {
        2048
    }
    fn send_ty<T: Serialize>(&mut self, _: &HeaderSeq, _: &T) -> Result<(), ()> {
        Ok(())
    }
    fn send_raw(&mut self, _: &HeaderSeq, _: &[u8]) -> Result<(), ()> {
        Ok(())
    }
    fn send_err(&mut self, _: &HeaderSeq, _: ProtocolError) -> Result<(), ()> {
        Ok(())
    }
}

struct MockInterface;
impl Interface for MockInterface {
    type Sink = NullSink;
}

type TestRouter = Router<MockInterface, rand::rngs::StdRng, 8, 8, 4>;
type TestStack = ArcNetStack<CriticalSectionRawMutex, TestRouter>;

fn rng(seed: u8) -> rand::rngs::StdRng {
    rand::rngs::StdRng::from_seed([seed; 32])
}

fn add_downstream(stack: &TestStack, net_id: u16) -> u8 {
    let ident = stack
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    stack
        .manage_profile(|im| im.reassign_interface_net_id(ident, net_id))
        .unwrap();
    ident
}

/// An upstream lease as it would arrive from the parent seed router.
fn upstream_grant(net_id: u16, expires: u16) -> SeedLease {
    SeedLease {
        net_id,
        refresh_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 42,
        },
        release_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 43,
        },
        expires_seconds: expires,
        max_refresh_seconds: 120,
        min_refresh_seconds: 62,
        refresh_token: (0x1122_3344_5566_7788u64 ^ expires as u64).to_le_bytes(),
    }
}

#[test]
fn delegation_upstream_only_on_bridges() {
    // A root router (no upstream) does not delegate.
    let root: TestStack = TestStack::new_with_profile(Router::new(rng(0)));
    assert_eq!(
        root.manage_profile(|im| im.seed_delegation_upstream()),
        None,
        "a root router must not delegate"
    );

    // A bridge router delegates to its upstream interface.
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(1), NullSink));
    assert_eq!(
        bridge.manage_profile(|im| im.seed_delegation_upstream()),
        Some(UPSTREAM_IDENT),
        "a bridge router delegates via UPSTREAM_IDENT"
    );
}

#[test]
fn register_delegated_seed_net_registers_and_shrinks_refresh_window() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(2), NullSink));
    // Downstream that the requester is reachable through (gets net_id=1).
    let down = add_downstream(&bridge, 1);
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();
    assert_eq!(source_net, 1);

    // Unknown source_net is rejected.
    assert_eq!(
        bridge.manage_profile(|im| im.register_delegated_seed_net(
            999,
            &upstream_grant(5, 30)
        )),
        Err(SeedAssignmentError::UnknownSource)
    );

    // Register the net leased from upstream (net_id=5) on behalf of source_net=1.
    let assignment = bridge
        .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(5, 30)))
        .expect("delegated registration should succeed");

    assert_eq!(assignment.net_id, 5);
    assert_eq!(assignment.expires_seconds, 30);
    assert_eq!(assignment.max_refresh_seconds, 120);
    // min_refresh is shrunk by the per-hop margin so a child refreshes inside
    // the parent's window (62 - 5 = 57).
    assert_eq!(assignment.min_refresh_seconds, 57);
    // The downstream is handed a *local* token, not the upstream's.
    assert_ne!(assignment.refresh_token, upstream_grant(5, 30).refresh_token);

    // The route now validates for its requester.
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            5,
            assignment.refresh_token
        )),
        Ok(DelegatedRefreshPreparation::Forward(upstream_grant(5, 30))),
        "the complete parent lease must be stored with the route"
    );
}

#[test]
fn prepare_delegated_refresh_checks_scope_token_and_existence() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(3), NullSink));
    let down = add_downstream(&bridge, 1);
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();

    let assignment = bridge
        .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(7, 30)))
        .unwrap();

    // Correct (scope, token) validates.
    assert!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            7,
            assignment.refresh_token
        ))
        .is_ok()
    );
    // Wrong token.
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(source_net, 7, [0xFF; 8])),
        Err(SeedRefreshError::BadRequest)
    );
    // Wrong requester (scope).
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net + 1,
            7,
            assignment.refresh_token
        )),
        Err(SeedRefreshError::BadRequest)
    );
    // Unknown net_id.
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            999,
            assignment.refresh_token
        )),
        Err(SeedRefreshError::UnknownNetId)
    );
}

#[test]
fn commit_delegated_refresh_extends_and_rotates_token() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(4), NullSink));
    let down = add_downstream(&bridge, 1);
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();

    let first = bridge
        .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(9, 30)))
        .unwrap();

    // Refresh with the refreshed upstream lease (now 120s).
    let refreshed = bridge
        .manage_profile(|im| {
            im.commit_delegated_refresh(source_net, first.refresh_token, &upstream_grant(9, 120))
        })
        .expect("delegated refresh should succeed");

    assert_eq!(refreshed.net_id, 9);
    assert_eq!(refreshed.expires_seconds, 120, "lease should track the upstream lease");
    assert_eq!(refreshed.min_refresh_seconds, 57);
    assert_ne!(
        refreshed.refresh_token, first.refresh_token,
        "the local token must rotate on refresh"
    );
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            9,
            refreshed.refresh_token
        )),
        Ok(DelegatedRefreshPreparation::Forward(upstream_grant(9, 120))),
        "commit must atomically replace the stored parent lease"
    );

    // The immediately previous token replays the committed response so a lost
    // response can be retried without another upstream refresh.
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            9,
            first.refresh_token
        )),
        Ok(DelegatedRefreshPreparation::Replay(refreshed.clone()))
    );
    assert!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(
            source_net,
            9,
            refreshed.refresh_token
        ))
        .is_ok()
    );

    // Refreshing an unknown net_id fails.
    assert_eq!(
        bridge.manage_profile(|im| im.commit_delegated_refresh(
            source_net,
            refreshed.refresh_token,
            &upstream_grant(123, 120)
        )),
        Err(SeedRefreshError::UnknownNetId)
    );
}

#[test]
fn re_delegation_of_same_net_is_idempotent() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(5), NullSink));
    let down = add_downstream(&bridge, 1);
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();

    let first = bridge
        .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(11, 30)))
        .unwrap();
    // Re-delegating the same net_id (e.g. the downstream re-requested) replaces
    // the stale entry rather than duplicating or erroring.
    let second = bridge
        .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(11, 30)))
        .expect("re-delegation should succeed");

    // Only the latest token is valid (the stale entry was removed).
    assert!(
        bridge
            .manage_profile(|im| im.prepare_delegated_refresh(source_net, 11, second.refresh_token))
            .is_ok()
    );
    assert_eq!(
        bridge.manage_profile(|im| im.prepare_delegated_refresh(source_net, 11, first.refresh_token)),
        Err(SeedRefreshError::BadRequest),
        "the superseded token must no longer validate"
    );
}

#[test]
fn can_delegate_seed_gates_unknown_source_and_full_table() {
    // S = 8 (route-table capacity) for TestRouter.
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(6), NullSink));
    let down = add_downstream(&bridge, 1);
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();

    // Unknown source is rejected before any upstream lease is requested.
    assert_eq!(
        bridge.manage_profile(|im| im.can_delegate_seed(source_net + 999)),
        Err(SeedAssignmentError::UnknownSource)
    );
    // A known source with room succeeds.
    assert_eq!(
        bridge.manage_profile(|im| im.can_delegate_seed(source_net)),
        Ok(())
    );

    // Fill the route table, then the pre-flight reports it full (so the
    // handler won't lease an upstream net_id it can't register).
    for net in 100u16..108 {
        bridge
            .manage_profile(|im| im.register_delegated_seed_net(source_net, &upstream_grant(net, 30)))
            .expect("registration should succeed until the table is full");
    }
    assert_eq!(
        bridge.manage_profile(|im| im.can_delegate_seed(source_net)),
        Err(SeedAssignmentError::NetIdsExhausted),
        "a full route table must be rejected up front"
    );
}

#[test]
fn delegated_parent_state_scales_with_route_capacity() {
    type LargeRouter = Router<MockInterface, rand::rngs::StdRng, 1, 40, 0>;
    type LargeStack = ArcNetStack<CriticalSectionRawMutex, LargeRouter>;

    let bridge: LargeStack =
        LargeStack::new_with_profile(Router::new_bridge(rng(7), NullSink));
    let down = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    bridge
        .manage_profile(|im| im.reassign_interface_net_id(down, 1))
        .unwrap();
    let source_net = bridge.manage_profile(|im| im.net_id_of(down)).unwrap();

    for net_id in 100u16..140 {
        let assignment = bridge
            .manage_profile(|im| {
                im.register_delegated_seed_net(source_net, &upstream_grant(net_id, 30))
            })
            .expect("every route up to S must carry its own parent lease");
        assert_eq!(
            bridge.manage_profile(|im| im.prepare_delegated_refresh(
                source_net,
                net_id,
                assignment.refresh_token,
            )),
            Ok(DelegatedRefreshPreparation::Forward(upstream_grant(
                net_id, 30
            )))
        );
    }

    assert_eq!(
        bridge.manage_profile(|im| im.can_delegate_seed(source_net)),
        Err(SeedAssignmentError::NetIdsExhausted)
    );
}
