//! `NetStack::wait_profile` wakes on every change of an interface's state,
//! whoever makes it: here the bus address claim service, which used to change
//! the state behind the back of anything watching the interface.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::time::Duration;

use ergot::{
    interface_manager::{
        Interface, InterfaceState, Profile,
        profiles::{direct_edge::DirectEdge, router::Router},
        utils::{
            framed_stream,
            std::{StdQueue, new_std_queue},
        },
    },
    net_stack::{ArcNetStack, services::bus_claim_with_retry},
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::{SeedableRng, rngs::StdRng};
use tokio::time::timeout;

struct BusInterface;
impl Interface for BusInterface {
    type Sink = framed_stream::Sink<StdQueue>;
}

type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<BusInterface>>;
type BusRouter = Router<BusInterface, StdRng, 4, 4, 4>;

fn sink() -> framed_stream::Sink<StdQueue> {
    framed_stream::Sink::new_from_handle(new_std_queue(4096), 250)
}

fn edge() -> EdgeStack {
    let stack = EdgeStack::new_with_profile(DirectEdge::new_target(sink()));
    stack
        .manage_profile(|p| p.set_interface_state((), InterfaceState::edge_link_local()))
        .unwrap();
    stack
}

/// Wait for the edge's node_id to differ from `known`.
async fn next_node(stack: &EdgeStack, known: Option<u8>) -> Option<u8> {
    let wait = stack.wait_profile(|p| {
        let node = p.interface_node_id(());
        (node != known).then_some(node)
    });
    timeout(Duration::from_secs(10), wait)
        .await
        .expect("the node change was not signalled")
}

/// A claim moves the device to its candidate and, when nobody answers and the
/// caller gives up, back to its previous address — without transmitting from
/// it. A watcher sees both moves as they happen.
#[tokio::test(start_paused = true)]
async fn a_watcher_sees_a_claim_try_its_candidate_and_give_it_back() {
    let stack = edge();
    let before = stack.manage_profile(|p| p.interface_node_id(()));

    let claim = async {
        let res = timeout(
            Duration::from_millis(100),
            bus_claim_with_retry(&stack, (), [42u8], 0xCCCC),
        )
        .await;
        assert!(res.is_err(), "nobody answers, so the claim times out");
    };
    let watch = async {
        let candidate = next_node(&stack, before).await;
        let restored = next_node(&stack, candidate).await;
        (candidate, restored)
    };
    let ((), (candidate, restored)) = tokio::join!(claim, watch);

    assert_eq!(candidate, Some(42));
    assert_eq!(restored, before);
}

#[tokio::test]
async fn a_condition_that_already_holds_returns_at_once() {
    let stack = edge();
    let node = stack.wait_profile(|p| p.interface_node_id(())).await;
    assert_eq!(
        Some(node),
        stack.manage_profile(|p| p.interface_node_id(()))
    );
}

#[test]
fn only_a_real_change_counts() {
    let mut edge = DirectEdge::<BusInterface>::new_target(sink());
    let generation = |p: &DirectEdge<BusInterface>| p.state_generation();

    let g0 = generation(&edge);
    edge.set_interface_state((), InterfaceState::edge_link_local())
        .unwrap();
    let g1 = generation(&edge);
    assert_ne!(g1, g0, "Down to link-local is a change");

    edge.set_interface_state((), InterfaceState::edge_link_local())
        .unwrap();
    assert_eq!(generation(&edge), g1, "the same state again is not");

    edge.set_interface_state(
        (),
        InterfaceState::Active {
            net_id: 0,
            node_id: 30,
        },
    )
    .unwrap();
    assert_ne!(generation(&edge), g1, "a new node_id is");
}

#[test]
fn a_router_counts_interfaces_coming_and_going_and_their_states() {
    let mut router = BusRouter::new(StdRng::seed_from_u64(0));
    let mut last = router.state_generation();
    let mut changed = |router: &BusRouter| {
        let now = router.state_generation();
        let changed = now != last;
        last = now;
        changed
    };

    let ident = router.register_interface(sink()).unwrap();
    assert!(changed(&router), "registering an interface");

    let pending = router.register_interface_pending(sink()).unwrap();
    assert!(changed(&router), "registering a pending interface");

    router.reassign_interface_net_id(pending, 77).unwrap();
    assert!(changed(&router), "assigning a net_id");

    router
        .set_interface_state(ident, InterfaceState::Inactive)
        .unwrap();
    assert!(changed(&router), "a state change");

    router
        .set_interface_state(ident, InterfaceState::Inactive)
        .unwrap();
    assert!(!changed(&router), "the same state again");

    router.deregister_interface(ident).unwrap();
    assert!(changed(&router), "removing an interface");
}
