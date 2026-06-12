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
    net_stack::{ArcNetStack, services::request_seed_lease},
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
    tokio::spawn({
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

    drop(held);
}
