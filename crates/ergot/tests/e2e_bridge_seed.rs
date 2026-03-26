//! E2E test: full bridge with seed routing.
//!
//! Topology:
//! ```text
//! Edge1 ←→ Bridge ←→ RootRouter ←→ Edge2
//! ```
//!
//! RootRouter is the seed router. Bridge requests a seed net_id for
//! Edge1's downstream interface. After assignment, Edge2 can ping
//! Edge1 using the globally-routable seed net_id.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::{pin::pin, time::Duration};

use bbqueue::traits::bbqhdl::BbqHandle;
use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::{
            direct_edge::{DirectEdge, EdgeFrameProcessor},
            router::{Router, UPSTREAM_IDENT},
        },
        transports::tokio_cobs_stream::{self, CobsStreamRxWorker, CobsStreamTxWorker},
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::{ArcNetStack, NetStackHandle, services::bridge_seed_assign},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    time::{sleep, timeout},
};

// Note: this test still manually constructs CobsStreamRxWorker/TxWorker for the
// bridge *downstream pending* case (RouterFrameProcessor::new(0) before seed assign).
// The bridge *upstream* uses tokio_cobs_stream::register_bridge_upstream.

type RootStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type BridgeStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;
type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

fn make_edge_stack() -> (EdgeStack, ergot::interface_manager::utils::std::StdQueue) {
    let queue = new_std_queue(4096);
    let stack = EdgeStack::new_with_profile(DirectEdge::new_target(
        cobs_stream::Sink::new_from_handle(queue.clone(), 512),
    ));
    (stack, queue)
}

fn spawn_ping_server(stack: &EdgeStack) {
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<ErgotPingEndpoint, 4>(Some("ping"));
            let server = pin!(server);
            let mut hdl = server.attach();
            loop {
                let _ = hdl
                    .serve(|val: &u32| {
                        let v = *val;
                        async move { v }
                    })
                    .await;
            }
        }
    });
}

async fn ping_with_retry<N: NetStackHandle + Clone>(stack: &N, addr: Address, val: u32) -> u32 {
    for _ in 0..30 {
        let result = timeout(
            Duration::from_millis(500),
            stack
                .stack()
                .endpoints()
                .request::<ErgotPingEndpoint>(addr, &val, Some("ping")),
        )
        .await;
        match result {
            Ok(Ok(v)) => return v,
            _ => sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("ping failed after retries");
}

async fn wait_active(stack: &EdgeStack) {
    for _ in 0..50 {
        let state = stack.manage_profile(|im| im.interface_state(()));
        if matches!(state, Some(InterfaceState::Active { .. })) {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("edge never reached Active state");
}

/// Register bridge upstream using the transport helper.
async fn register_bridge_upstream(
    stack: &BridgeStack,
    reader: impl AsyncRead + Unpin + Send + 'static,
    writer: impl AsyncWrite + Unpin + Send + 'static,
    queue: ergot::interface_manager::utils::std::StdQueue,
) {
    tokio_cobs_stream::register_bridge_upstream(stack.clone(), reader, writer, queue, None, None)
        .await
        .unwrap();
}

/// Wait for an interface to be Active, return its net_id.
async fn wait_interface_active(stack: &BridgeStack, ident: u8) -> u16 {
    for _ in 0..50 {
        let state = stack.manage_profile(|im| im.interface_state(ident));
        if let Some(InterfaceState::Active { net_id, .. }) = state {
            return net_id;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("interface {} never reached Active", ident);
}

#[tokio::test]
async fn bridge_seed_routing_e2e_ping() {
    // Topology: Edge1 ←→ Bridge ←→ RootRouter ←→ Edge2
    //
    // 1. Root assigns net_id=1 to Bridge (via direct link)
    // 2. Root assigns net_id=2 to Edge2 (via direct link)
    // 3. Bridge requests seed net_id from Root for Edge1's downstream
    // 4. Root assigns seed net_id=3 (routed through Bridge's link)
    // 5. Bridge reassigns Edge1's downstream to net_id=3
    // 6. Edge2 pings Edge1 at (3.2:0) → Root routes to Bridge → Bridge routes to Edge1

    let _ = env_logger::builder().is_test(true).try_init();

    // Create stacks
    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));
    let root_stack: RootStack = RootStack::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    // Duplex channels
    let (bridge_up_read, root_d0_write) = tokio::io::duplex(8192);
    let (root_d0_read, bridge_up_write) = tokio::io::duplex(8192);
    let (e1_read, bridge_d0_write) = tokio::io::duplex(8192);
    let (bridge_d0_read, e1_write) = tokio::io::duplex(8192);
    let (e2_read, root_d1_write) = tokio::io::duplex(8192);
    let (root_d1_read, e2_write) = tokio::io::duplex(8192);

    // Start seed router handler on root
    tokio::spawn({
        let root = root_stack.clone();
        async move {
            root.services().seed_router_request_handler::<4>().await;
        }
    });

    // Register root downstream[0] → bridge upstream (net_id=1)
    tokio_cobs_stream::register_router(
        root_stack.clone(),
        root_d0_read,
        root_d0_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // Register root downstream[1] → edge2 (net_id=2)
    tokio_cobs_stream::register_router(
        root_stack.clone(),
        root_d1_read,
        root_d1_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // Register bridge upstream
    register_bridge_upstream(
        &bridge_stack,
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
    )
    .await;

    // Register bridge downstream[0] → edge1
    // Use register_interface_pending (no auto-assigned net_id) to avoid
    // collision with upstream's net_id.
    let bridge_d0_queue = new_std_queue(4096);
    let bridge_d0_ident = bridge_stack
        .manage_profile(|im| {
            im.register_interface_pending(cobs_stream::Sink::new_from_handle(
                bridge_d0_queue.clone(),
                512,
            ))
        })
        .unwrap();

    // Spawn transport workers for bridge downstream[0]
    {
        let closer = std::sync::Arc::new(maitake_sync::WaitQueue::new());
        bridge_stack.manage_profile(|im| {
            im.set_interface_closer(bridge_d0_ident, closer.clone());
        });

        let stack_clone = bridge_stack.clone();
        let mut rx_worker = CobsStreamRxWorker {
            nsh: bridge_stack.clone(),
            reader: Box::new(bridge_d0_read) as Box<dyn AsyncRead + Unpin + Send>,
            closer: closer.clone(),
            processor: ergot::interface_manager::profiles::router::RouterFrameProcessor::new(0), // placeholder, will be updated
            ident: bridge_d0_ident,
            liveness: None,
            state_notify: None,
            cobs_buf_size: 1024 * 1024,
        };

        tokio::task::spawn(async move {
            let close = rx_worker.closer.clone();
            select! {
                _run = rx_worker.run() => { close.close(); },
                _clf = close.wait() => {},
            }
            stack_clone.manage_profile(|im| {
                let _ = im.deregister_interface(bridge_d0_ident);
            });
        });
        tokio::task::spawn(
            CobsStreamTxWorker {
                writer: Box::new(bridge_d0_write) as Box<dyn AsyncWrite + Unpin + Send>,
                consumer:
                    <ergot::interface_manager::utils::std::StdQueue as BbqHandle>::stream_consumer(
                        &bridge_d0_queue,
                    ),
                closer: closer.clone(),
            }
            .run(),
        );
    }

    // Register edge1 as target of bridge
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge1_stack.clone(),
        e1_read,
        e1_write,
        edge1_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Inactive,
        None,
        None,
    )
    .await
    .unwrap();

    // Register edge2 as target of root
    tokio_cobs_stream::register_edge::<_, TokioStreamInterface, _, _>(
        edge2_stack.clone(),
        e2_read,
        e2_write,
        edge2_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Inactive,
        None,
        None,
    )
    .await
    .unwrap();

    // Start ping servers
    spawn_ping_server(&edge1_stack);
    spawn_ping_server(&edge2_stack);

    // Bootstrap: root pings edge2 to activate it
    let edge2_addr = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };
    ping_with_retry(&root_stack, edge2_addr, 0).await;
    wait_active(&edge2_stack).await;

    // Bootstrap bridge upstream: root pings the bridge address (net_id=1, node=2)
    // This sends a frame to bridge's upstream, triggering net_id discovery
    let bridge_addr_from_root = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0, // wildcard
    };
    // The ping will fail (bridge has no ping server) but the frame activates upstream
    let _ = timeout(
        Duration::from_millis(500),
        root_stack.endpoints().request::<ErgotPingEndpoint>(
            bridge_addr_from_root,
            &0u32,
            Some("ping"),
        ),
    )
    .await;

    // Wait for bridge upstream to become Active
    let _bridge_upstream_net = wait_interface_active(&bridge_stack, UPSTREAM_IDENT).await;

    // === KEY PART: Bridge requests seed net_id from root ===
    let lease = bridge_seed_assign(&bridge_stack, UPSTREAM_IDENT, bridge_d0_ident)
        .await
        .expect("seed assignment should succeed");

    // The seed net_id should be 3 (root already assigned 1 and 2)
    assert_eq!(lease.net_id, 3, "seed net_id should be 3");

    // Verify bridge's downstream was reassigned
    let new_net = bridge_stack.manage_profile(|im| im.interface_state(bridge_d0_ident));
    assert!(
        matches!(new_net, Some(InterfaceState::Active { net_id: 3, .. })),
        "bridge downstream should be net_id=3, got {:?}",
        new_net
    );

    // Bootstrap edge1: now that bridge downstream has net_id=3, ping edge1 to activate it
    let edge1_global_addr = Address {
        network_id: 3,
        node_id: 2,
        port_id: 0,
    };
    // Root pings edge1 through bridge (seed route)
    ping_with_retry(&root_stack, edge1_global_addr, 0).await;
    wait_active(&edge1_stack).await;

    // === E2E: root can ping edge1 through bridge via seed route ===
    let response = ping_with_retry(&root_stack, edge1_global_addr, 42).await;
    assert_eq!(response, 42, "root→bridge→edge1 ping should work");

    // === E2E: edge2 pings edge1 through root→bridge ===
    let response = ping_with_retry(&edge2_stack, edge1_global_addr, 99).await;
    assert_eq!(response, 99, "edge2→root→bridge→edge1 ping should work");
}
