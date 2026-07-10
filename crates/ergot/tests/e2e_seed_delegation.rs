//! E2E: hierarchical seed delegation.
//!
//! A bridge running `seed_router_request_handler` that has an upstream must
//! **delegate** a downstream's seed request to the root, not allocate from its
//! own pool — otherwise two allocators share one 16-bit space and nested
//! assignments collide (the original "1.2" bug).
//!
//! ```text
//!   Requester ──(cobs)── Bridge ──(cobs, upstream)── Root
//! ```
//!
//! The test makes root's next-free net_id (5) differ from the bridge's (3) by
//! pre-consuming net_ids on each, then asserts the requester's delegated net_id
//! is the **root**-allocated 5. Local allocation (the bug) would hand out the
//! bridge's 3.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use std::time::Duration;

use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::router::{Router, UPSTREAM_IDENT},
    },
    net_stack::{
        ArcNetStack,
        services::{bridge_seed_refresh, request_seed_lease},
    },
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

type RouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

async fn wait_interface_active(stack: &RouterStack, ident: u8) -> u16 {
    for _ in 0..60 {
        if let Some(InterfaceState::Active { net_id, .. }) =
            stack.manage_profile(|im| im.interface_state(ident))
        {
            return net_id;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("interface {ident} never became Active");
}

/// Register a downstream on `stack` whose far end is held alive but never
/// written, so the interface stays registered (consuming a net_id) without
/// carrying traffic.
async fn consume_net_id(stack: &RouterStack, held: &mut Vec<tokio::io::DuplexStream>) {
    let (near_read, far_write) = tokio::io::duplex(64);
    let (far_read, near_write) = tokio::io::duplex(64);
    ergot::interface_manager::transports::tokio_cobs_stream::register_router(
        stack.clone(),
        near_read,
        near_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();
    held.push(far_write);
    held.push(far_read);
}

#[tokio::test]
async fn bridge_delegates_downstream_request_to_root() {
    let _ = env_logger::builder().is_test(true).try_init();

    use ergot::interface_manager::transports::tokio_cobs_stream as tcs;
    use ergot::interface_manager::utils::{cobs_stream, std::new_std_queue};

    // ---- Stacks ----
    let root: RouterStack = RouterStack::new();

    let bridge_up_queue = new_std_queue(4096);
    let bridge: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let requester_up_queue = new_std_queue(4096);
    let requester: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(requester_up_queue.clone(), 512),
    ));

    // ---- Seed handlers on root AND bridge (bridge will delegate) ----
    tokio::spawn({
        let s = root.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });
    tokio::spawn({
        let s = root.clone();
        async move { s.services().ping_handler::<4>().await }
    });
    let bridge_seed_handler = tokio::spawn({
        let s = bridge.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });

    // ---- root ⟷ bridge upstream link (root assigns the bridge link net_id=1) ----
    let (bridge_up_read, root_b_write) = tokio::io::duplex(8192);
    let (root_b_read, bridge_up_write) = tokio::io::duplex(8192);
    tcs::register_router(root.clone(), root_b_read, root_b_write, 512, 4096, None, None)
        .await
        .unwrap();
    tcs::register_bridge_upstream(
        bridge.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    // Pre-consume net_ids so root's next-free (5) differs from the bridge's (3):
    // root holds nets 2,3,4 in addition to the bridge link (1).
    let mut held = Vec::new();
    consume_net_id(&root, &mut held).await; // net 2
    consume_net_id(&root, &mut held).await; // net 3
    consume_net_id(&root, &mut held).await; // net 4

    // Bootstrap the bridge upstream: a frame from root makes it discover net 1.
    let _ = timeout(
        Duration::from_millis(500),
        root.endpoints()
            .request::<ErgotPingEndpoint>(Address { network_id: 1, node_id: 2, port_id: 0 }, &0u32, Some("ping")),
    )
    .await;
    let bridge_up_net = wait_interface_active(&bridge, UPSTREAM_IDENT).await;
    assert_eq!(bridge_up_net, 1, "bridge upstream should be root-assigned net 1");

    // ---- bridge ⟷ requester link. Registered now (after the bridge upstream
    // is net 1), so it gets the bridge's net 2; the bridge's next-free is 3. ----
    let (req_up_read, bridge_r_write) = tokio::io::duplex(8192);
    let (bridge_r_read, req_up_write) = tokio::io::duplex(8192);
    let bridge_down = tcs::register_router(
        bridge.clone(),
        bridge_r_read,
        bridge_r_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        bridge.manage_profile(|im| im.net_id_of(bridge_down)),
        Some(2),
        "bridge's requester link should be net 2 (next-free is then 3)"
    );
    tcs::register_bridge_upstream(
        requester.clone(),
        req_up_read,
        req_up_write,
        requester_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    // ---- The requester asks the bridge for a seed net_id ----
    let lease = timeout(
        Duration::from_secs(5),
        request_seed_lease(&requester, UPSTREAM_IDENT),
    )
    .await
    .expect("seed lease timed out — the bridge did not answer")
    .expect("seed lease request failed");

    // Delegation => root allocated it (root's next-free is 5).
    // The local-allocation bug would have handed out the bridge's next-free, 3.
    assert_eq!(
        lease.net_id, 5,
        "delegated net_id must come from the root's pool (5), not the bridge's local pool (3)"
    );

    // The parent lease is profile-owned, not task-local: restarting the
    // handler must not lose the upstream token needed to refresh this route.
    bridge_seed_handler.abort();
    let _ = bridge_seed_handler.await;
    let replacement_handler = tokio::spawn({
        let s = bridge.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });
    let refreshed = timeout(
        Duration::from_secs(5),
        bridge_seed_refresh(&requester, &lease),
    )
    .await
    .expect("seed refresh timed out after handler restart")
    .expect("seed refresh failed after handler restart");
    assert_eq!(refreshed.net_id, lease.net_id);
    assert_ne!(refreshed.refresh_token, lease.refresh_token);
    replacement_handler.abort();

    drop(held);
}

/// The previous test proves the delegated net_id comes from the root's pool.
/// This one proves the resulting route is actually *routable*: a ping from the
/// root reaches an edge attached behind the requester, traversing both
/// delegated seed-route hops (Root→Bridge→Requester→Edge).
///
/// ```text
///   Edge ──(cobs)── Requester ──(cobs)── Bridge ──(cobs, upstream)── Root
/// ```
#[tokio::test]
async fn delegated_route_is_routable_end_to_end() {
    let _ = env_logger::builder().is_test(true).try_init();

    use common::{make_edge_stack, ping_with_retry, spawn_ping_server, wait_active};
    use ergot::interface_manager::profiles::direct_edge::EdgeFrameProcessor;
    use ergot::interface_manager::transports::tokio_cobs_stream as tcs;
    use ergot::interface_manager::utils::{cobs_stream, std::new_std_queue};
    use ergot::net_stack::services::bridge_seed_assign;

    // ---- Stacks ----
    let root: RouterStack = RouterStack::new();

    let bridge_up_queue = new_std_queue(4096);
    let bridge: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let requester_up_queue = new_std_queue(4096);
    let requester: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(requester_up_queue.clone(), 512),
    ));

    let (edge, edge_queue) = make_edge_stack();

    // ---- Seed handlers on root AND bridge (bridge delegates) ----
    tokio::spawn({
        let s = root.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });
    tokio::spawn({
        let s = bridge.clone();
        async move { s.services().seed_router_request_handler::<4>().await }
    });

    // ---- root ⟷ bridge upstream (root assigns the bridge link net_id=1) ----
    let (bridge_up_read, root_b_write) = tokio::io::duplex(8192);
    let (root_b_read, bridge_up_write) = tokio::io::duplex(8192);
    tcs::register_router(root.clone(), root_b_read, root_b_write, 512, 4096, None, None)
        .await
        .unwrap();
    tcs::register_bridge_upstream(
        bridge.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    // Pre-consume net_ids on root (it holds 2,3,4 besides the bridge link 1) so
    // the delegated net (root's next-free, 5) cannot coincide with a
    // bridge-local link net — the bridge↔requester link below is net 2.
    let mut held = Vec::new();
    consume_net_id(&root, &mut held).await; // net 2
    consume_net_id(&root, &mut held).await; // net 3
    consume_net_id(&root, &mut held).await; // net 4

    // Bootstrap the bridge upstream.
    let _ = timeout(
        Duration::from_millis(500),
        root.endpoints().request::<ErgotPingEndpoint>(
            Address { network_id: 1, node_id: 2, port_id: 0 },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(wait_interface_active(&bridge, UPSTREAM_IDENT).await, 1);

    // ---- bridge ⟷ requester link (bridge-local net 2), registered after the
    // bridge upstream is net 1 ----
    let (req_up_read, bridge_r_write) = tokio::io::duplex(8192);
    let (bridge_r_read, req_up_write) = tokio::io::duplex(8192);
    let bridge_down =
        tcs::register_router(bridge.clone(), bridge_r_read, bridge_r_write, 512, 4096, None, None)
            .await
            .unwrap();
    assert_eq!(bridge.manage_profile(|im| im.net_id_of(bridge_down)), Some(2));
    tcs::register_bridge_upstream(
        requester.clone(),
        req_up_read,
        req_up_write,
        requester_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    // ---- requester ⟷ edge link (pending; the seed assign gives it the
    // delegated net) ----
    let (edge_read, req_d_write) = tokio::io::duplex(8192);
    let (req_d_read, edge_write) = tokio::io::duplex(8192);
    let req_down = tcs::register_bridge_downstream(
        requester.clone(),
        req_d_read,
        req_d_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();
    tcs::register_edge::<_, TokioStreamInterface, _, _>(
        edge.clone(),
        edge_read,
        edge_write,
        edge_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Inactive,
        None,
        None,
    )
    .await
    .unwrap();
    spawn_ping_server(&edge);

    // Bootstrap the requester upstream: the bridge pings the requester's
    // upstream node so it discovers net 2.
    let _ = timeout(
        Duration::from_millis(500),
        bridge.endpoints().request::<ErgotPingEndpoint>(
            Address { network_id: 2, node_id: 2, port_id: 0 },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(wait_interface_active(&requester, UPSTREAM_IDENT).await, 2);

    // ---- The requester asks the bridge for a seed net for its edge link; the
    // bridge delegates to root → root allocates net 5 and the requester
    // reassigns the edge link to it ----
    let lease = timeout(
        Duration::from_secs(5),
        bridge_seed_assign(&requester, UPSTREAM_IDENT, req_down),
    )
    .await
    .expect("seed assign timed out")
    .expect("seed assign failed");
    assert_eq!(lease.net_id, 5, "delegated net must come from the root's pool");
    assert!(
        matches!(
            requester.manage_profile(|im| im.interface_state(req_down)),
            Some(InterfaceState::Active { net_id: 5, .. })
        ),
        "the requester's edge link should be reassigned to the delegated net 5"
    );

    // ---- E2E: root pings the edge across both delegated hops ----
    let edge_addr = Address { network_id: 5, node_id: 2, port_id: 0 };
    ping_with_retry(&root, edge_addr, 0).await; // bootstrap the edge
    wait_active(&edge).await;
    let resp = ping_with_retry(&root, edge_addr, 42).await;
    assert_eq!(
        resp, 42,
        "root must reach the edge Root→Bridge→Requester→Edge via the delegated seed routes"
    );

    drop(held);
}
