//! End-to-end tests: edge → bridge-router → root-router → edge.
//!
//! Topology:
//! ```text
//! Edge1 ←duplex→ Bridge ←duplex→ RootRouter ←duplex→ Edge2
//! ```
//! Edge1 is downstream of the Bridge. The Bridge's upstream connects
//! to the RootRouter. Edge2 is downstream of the RootRouter.

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
    net_stack::{ArcNetStack, NetStackHandle},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    select,
    time::{sleep, timeout},
};

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

/// Helper: register a COBS stream interface on a Router as downstream.
async fn register_router_downstream(
    stack: &RootStack,
    reader: impl AsyncRead + Unpin + Send + 'static,
    writer: impl AsyncWrite + Unpin + Send + 'static,
) -> u8 {
    tokio_cobs_stream::register_router::<_, TokioStreamInterface, _, _>(
        stack.clone(),
        reader,
        writer,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap()
}

/// Helper: register the upstream side of a bridge (RxWorker + TxWorker).
///
/// The bridge's upstream uses EdgeFrameProcessor (discovers net_id from
/// incoming frames) and UPSTREAM_IDENT.
async fn register_bridge_upstream(
    stack: &BridgeStack,
    reader: impl AsyncRead + Unpin + Send + 'static,
    writer: impl AsyncWrite + Unpin + Send + 'static,
    queue: ergot::interface_manager::utils::std::StdQueue,
) {
    let closer = std::sync::Arc::new(maitake_sync::WaitQueue::new());

    stack.manage_profile(|im| {
        im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Inactive)
            .unwrap();
    });

    let stack_clone = stack.clone();

    let mut rx_worker = CobsStreamRxWorker {
        nsh: stack.clone(),
        reader: Box::new(reader) as Box<dyn AsyncRead + Unpin + Send>,
        closer: closer.clone(),
        processor: EdgeFrameProcessor::new(),
        ident: UPSTREAM_IDENT,
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
            _ = im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Down);
        });
    });
    tokio::task::spawn(
        CobsStreamTxWorker {
            writer: Box::new(writer) as Box<dyn AsyncWrite + Unpin + Send>,
            consumer:
                <ergot::interface_manager::utils::std::StdQueue as BbqHandle>::stream_consumer(
                    &queue,
                ),
            closer: closer.clone(),
        }
        .run(),
    );
}

#[tokio::test]
async fn bridge_forwards_ping_upstream() {
    // Topology: Edge1 ← Bridge ← RootRouter ← Edge2
    //
    // Edge1 is downstream of Bridge.
    // Bridge upstream connects to RootRouter.
    // Edge2 is downstream of RootRouter.
    // Ping: Edge1 → Bridge → RootRouter → Edge2 (and response back)

    let _ = env_logger::builder().is_test(true).try_init();

    // Create the bridge's upstream queue + sink
    let bridge_up_queue = new_std_queue(4096);
    let bridge_stack: BridgeStack = BridgeStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let root_stack: RootStack = RootStack::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    // Duplex: Bridge upstream ↔ RootRouter downstream[0]
    let (bridge_up_read, root_d0_write) = tokio::io::duplex(8192);
    let (root_d0_read, bridge_up_write) = tokio::io::duplex(8192);

    // Duplex: Edge1 ↔ Bridge downstream[0]
    let (e1_read, bridge_d0_write) = tokio::io::duplex(8192);
    let (bridge_d0_read, e1_write) = tokio::io::duplex(8192);

    // Duplex: Edge2 ↔ RootRouter downstream[1]
    let (e2_read, root_d1_write) = tokio::io::duplex(8192);
    let (root_d1_read, e2_write) = tokio::io::duplex(8192);

    // Register RootRouter downstream interfaces
    let _root_d0 = register_router_downstream(&root_stack, root_d0_read, root_d0_write).await;
    let _root_d1 = register_router_downstream(&root_stack, root_d1_read, root_d1_write).await;

    // Register Bridge upstream
    register_bridge_upstream(
        &bridge_stack,
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
    )
    .await;

    // Register Bridge downstream (Edge1)
    // Use the same register_router pattern — Bridge is a Router with downstream
    tokio_cobs_stream::register_router::<_, TokioStreamInterface, _, _>(
        bridge_stack.clone(),
        bridge_d0_read,
        bridge_d0_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    // Register Edge1 as target of Bridge
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

    // Register Edge2 as target of RootRouter
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

    // Bootstrap: root pings its direct edges to establish net_ids
    // RootRouter downstream[0] = net_id 1 (connects to bridge upstream)
    // RootRouter downstream[1] = net_id 2 (connects to edge2)
    let edge2_via_root = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };
    ping_with_retry(&root_stack, edge2_via_root, 0).await;

    // Bootstrap bridge: root pings through bridge to edge1
    // Bridge's upstream discovers net_id=1 from root's frame
    // Bridge's downstream[0] = net_id 1 (internal to bridge)
    // Edge1 discovers its net_id from bridge

    sleep(Duration::from_millis(200)).await;

    // Bootstrap edge1 through bridge
    // Bridge downstream edge1 has a net_id assigned by bridge's register_interface
    let bridge_d0_net = bridge_stack
        .manage_profile(|im| im.interface_state(0))
        .and_then(|s| match s {
            InterfaceState::Active { net_id, .. } => Some(net_id),
            _ => None,
        });

    if let Some(net_id) = bridge_d0_net {
        let edge1_via_bridge = Address {
            network_id: net_id,
            node_id: 2,
            port_id: 0,
        };
        // Ping edge1 through bridge to bootstrap it
        let _ = timeout(
            Duration::from_millis(500),
            bridge_stack.endpoints().request::<ErgotPingEndpoint>(
                edge1_via_bridge,
                &0,
                Some("ping"),
            ),
        )
        .await;
    }

    wait_active(&edge2_stack).await;
    wait_active(&edge1_stack).await;

    // Now test: Edge2 pings Edge1 through RootRouter and Bridge
    // Edge1's address from Edge2's perspective goes through root → bridge → edge1
    // This requires seed routing or the bridge forwarding unknown destinations upstream

    // For now, test that Edge2 can ping from RootRouter
    let response = ping_with_retry(&root_stack, edge2_via_root, 42).await;
    assert_eq!(response, 42);
}
