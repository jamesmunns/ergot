//! A bridge upstream reverts to its link-local boot state on a liveness
//! timeout, rather than going `Inactive`, so its transmit side stays ungated
//! and can re-provoke net_id discovery. See
//! [`RxWorker::revert_to_link_local_on_timeout`], which
//! [`register_bridge_upstream`] enables.
//!
//! The default (`Inactive`) policy for ordinary interfaces is covered by
//! `e2e_stream::liveness_timeout_on_disconnect`.
//!
//! Composition with the `EdgeFrameProcessor` (re)discovery guards — a
//! frame arriving while the interface is link-local re-discovers or reactivates
//! without hijacking another segment's net_id — is covered by
//! `edge_rediscovery.rs`, which drives the processor from the same `Active { net_id: 0 }`
//! boot state.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::time::Duration;

use ergot::{
    Address,
    interface_manager::{
        InterfaceState, LivenessConfig, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::router::{Router, UPSTREAM_IDENT},
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::ArcNetStack,
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

type RootStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type BridgeStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

fn upstream_state(stack: &BridgeStack) -> Option<InterfaceState> {
    stack.manage_profile(|im| im.interface_state(UPSTREAM_IDENT))
}

#[tokio::test]
async fn bridge_upstream_liveness_reverts_to_link_local() {
    let _ = env_logger::builder().is_test(true).try_init();
    let liveness = LivenessConfig { timeout_ms: 500 };

    let root_stack: RootStack = RootStack::new();
    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let (bridge_up_read, root_d_write) = tokio::io::duplex(8192);
    let (root_d_read, bridge_up_write) = tokio::io::duplex(8192);

    // Root's downstream carries the bridge; the bridge upstream discovers its
    // net_id from frames the root addresses to it.
    tokio_cobs_stream::register_router(
        root_stack.clone(),
        root_d_read,
        root_d_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // The bridge answers pings so the bootstrap request completes.
    tokio::spawn({
        let s = bridge_stack.clone();
        async move { s.services().ping_handler::<4>().await }
    });

    // Registered with liveness and the revert-to-link-local policy (the latter
    // is set for us by register_bridge_upstream).
    tokio_cobs_stream::register_bridge_upstream(
        bridge_stack.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        Some(liveness),
        None,
    )
    .await
    .unwrap();

    // Starts link-local.
    assert!(
        matches!(
            upstream_state(&bridge_stack),
            Some(InterfaceState::Active { net_id: 0, .. })
        ),
        "upstream should start link-local, got {:?}",
        upstream_state(&bridge_stack)
    );

    // Provoke discovery: the root pings the bridge, so the upstream sees a frame
    // addressed to it (net 1, EDGE_NODE_ID) and adopts a real net_id.
    let _ = timeout(
        Duration::from_millis(500),
        root_stack.endpoints().request::<ErgotPingEndpoint>(
            Address {
                network_id: 1,
                node_id: 2,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;

    // Upstream should have discovered a real (non-link-local) net_id.
    let mut discovered = false;
    for _ in 0..40 {
        if let Some(InterfaceState::Active { net_id, .. }) = upstream_state(&bridge_stack) {
            if net_id != 0 {
                discovered = true;
                break;
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(
        discovered,
        "upstream should discover a real net_id, got {:?}",
        upstream_state(&bridge_stack)
    );

    // Go quiet: no more frames reach the upstream. After the liveness timeout it
    // must revert to the link-local boot state, not go Inactive.
    sleep(Duration::from_millis(900)).await;

    assert_eq!(
        upstream_state(&bridge_stack),
        Some(InterfaceState::edge_link_local()),
        "upstream should revert to link-local on liveness timeout, not Inactive",
    );
}
