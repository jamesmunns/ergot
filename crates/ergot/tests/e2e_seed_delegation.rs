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
//! Every bridge downlink starts pending and receives its net_id from the root,
//! so there is only one allocator for the whole hierarchy.

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

    // Bootstrap the bridge upstream: a frame from root makes it discover net 1.
    let _ = timeout(
        Duration::from_millis(500),
        root.endpoints()
            .request::<ErgotPingEndpoint>(Address { network_id: 1, node_id: 2, port_id: 0 }, &0u32, Some("ping")),
    )
    .await;
    let bridge_up_net = wait_interface_active(&bridge, UPSTREAM_IDENT).await;
    assert_eq!(bridge_up_net, 1, "bridge upstream should be root-assigned net 1");

    // ---- bridge ⟷ requester link. It starts pending, then root assigns net 2. ----
    let (req_up_read, bridge_r_write) = tokio::io::duplex(8192);
    let (bridge_r_read, req_up_write) = tokio::io::duplex(8192);
    let bridge_down = tcs::register_bridge_downstream(
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
    let bridge_link = root
        .manage_profile(|im| im.request_seed_net_assign(1))
        .unwrap();
    bridge
        .manage_profile(|im| im.reassign_interface_net_id(bridge_down, bridge_link.net_id))
        .unwrap();
    assert_eq!(
        bridge.manage_profile(|im| im.net_id_of(bridge_down)),
        Some(2),
        "bridge's requester link should use root-assigned net 2"
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

    let _ = timeout(
        Duration::from_millis(500),
        bridge.endpoints().request::<ErgotPingEndpoint>(
            Address {
                network_id: 2,
                node_id: 2,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(wait_interface_active(&requester, UPSTREAM_IDENT).await, 2);

    // ---- The requester asks the bridge for a seed net_id ----
    let lease = timeout(
        Duration::from_secs(5),
        request_seed_lease(&requester, UPSTREAM_IDENT),
    )
    .await
    .expect("seed lease timed out — the bridge did not answer")
    .expect("seed lease request failed");

    // Delegation => root allocated the next globally unique net.
    assert_eq!(
        lease.net_id, 3,
        "delegated net_id must come from the root's pool"
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

    // ---- bridge ⟷ requester link (root-assigned net 2) ----
    let (req_up_read, bridge_r_write) = tokio::io::duplex(8192);
    let (bridge_r_read, req_up_write) = tokio::io::duplex(8192);
    let bridge_down =
        tcs::register_bridge_downstream(bridge.clone(), bridge_r_read, bridge_r_write, 512, 4096, None, None)
            .await
            .unwrap();
    let bridge_link = root
        .manage_profile(|im| im.request_seed_net_assign(1))
        .unwrap();
    bridge
        .manage_profile(|im| im.reassign_interface_net_id(bridge_down, bridge_link.net_id))
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
    // bridge delegates to root → root allocates net 3 and the requester
    // reassigns the edge link to it ----
    let lease = timeout(
        Duration::from_secs(5),
        bridge_seed_assign(&requester, UPSTREAM_IDENT, req_down),
    )
    .await
    .expect("seed assign timed out")
    .expect("seed assign failed");
    assert_eq!(lease.net_id, 3, "delegated net must come from the root's pool");
    assert!(
        matches!(
            requester.manage_profile(|im| im.interface_state(req_down)),
            Some(InterfaceState::Active { net_id: 3, .. })
        ),
        "the requester's edge link should be reassigned to delegated net 3"
    );

    // ---- E2E: root pings the edge across both delegated hops ----
    let edge_addr = Address { network_id: 3, node_id: 2, port_id: 0 };
    ping_with_retry(&root, edge_addr, 0).await; // bootstrap the edge
    wait_active(&edge).await;
    let resp = ping_with_retry(&root, edge_addr, 42).await;
    assert_eq!(
        resp, 42,
        "root must reach the edge Root→Bridge→Requester→Edge via the delegated seed routes"
    );

}
