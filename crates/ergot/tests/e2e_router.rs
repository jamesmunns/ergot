//! End-to-end tests: two DirectEdge stacks communicating through a DirectRouter.
//!
//! Uses `tokio::io::duplex()` to create in-memory bidirectional pipes.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::{pin::pin, time::Duration};

use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::{DirectEdge, EdgeFrameProcessor},
        profiles::direct_router::DirectRouter,
        profiles::router::Router,
        transports::tokio_cobs_stream,
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::{ArcNetStack, NetStackHandle},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

type RouterStackTy = ArcNetStack<CriticalSectionRawMutex, DirectRouter<TokioStreamInterface>>;
type EdgeStackTy = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

fn make_edge_stack() -> (EdgeStackTy, ergot::interface_manager::utils::std::StdQueue) {
    let queue = new_std_queue(4096);
    let stack = EdgeStackTy::new_with_profile(DirectEdge::new_target(
        cobs_stream::Sink::new_from_handle(queue.clone(), 512),
    ));
    (stack, queue)
}

fn spawn_ping_server(stack: &EdgeStackTy) {
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

/// Wait for an edge stack to reach Active state.
async fn wait_active(stack: &EdgeStackTy) {
    for _ in 0..50 {
        let state = stack.manage_profile(|im| im.interface_state(()));
        if matches!(state, Some(InterfaceState::Active { .. })) {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("edge never reached Active state");
}

#[tokio::test]
async fn edge_to_edge_through_router() {
    //  Edge1 <--duplex1--> Router <--duplex2--> Edge2
    let _ = env_logger::builder().is_test(true).try_init();

    // Create stacks
    let router_stack: RouterStackTy = RouterStackTy::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    // Create in-memory duplex channels
    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);

    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

    // Register router interfaces (each gets a net_id)
    let ident1 = tokio_cobs_stream::register_router(
        router_stack.clone(),
        r1_read,
        r1_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    let ident2 = tokio_cobs_stream::register_router(
        router_stack.clone(),
        r2_read,
        r2_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    assert_ne!(ident1, ident2);

    // Register edge1 as target
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

    // Register edge2 as target
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

    // Start ping servers on both edges so the router can bootstrap them
    spawn_ping_server(&edge1_stack);
    spawn_ping_server(&edge2_stack);

    // Bootstrap: ping edges from the router to trigger net_id discovery.
    // The router knows the net_ids (1 and 2), so it can address the edges directly.
    let edge1_addr = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0,
    };
    let edge2_addr = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };

    // Send pings from router to bootstrap both edges
    ping_with_retry(&router_stack, edge1_addr, 0).await;
    ping_with_retry(&router_stack, edge2_addr, 0).await;

    // Now edges should be Active
    wait_active(&edge1_stack).await;
    wait_active(&edge2_stack).await;

    // Ping edge2 from edge1, through the router
    let response = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(response, 42);

    // Second ping
    let response = ping_with_retry(&edge1_stack, edge2_addr, 100).await;
    assert_eq!(response, 100);
}

#[tokio::test]
async fn bidirectional_through_router() {
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: RouterStackTy = RouterStackTy::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);
    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r1_read,
        r1_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r2_read,
        r2_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

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

    // Ping servers on both edges
    spawn_ping_server(&edge1_stack);
    spawn_ping_server(&edge2_stack);

    // Bootstrap edges from router
    let edge1_addr = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0,
    };
    let edge2_addr = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };

    ping_with_retry(&router_stack, edge1_addr, 0).await;
    ping_with_retry(&router_stack, edge2_addr, 0).await;

    wait_active(&edge1_stack).await;
    wait_active(&edge2_stack).await;

    // Edge1 → Edge2
    let r = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(r, 42);

    // Edge2 → Edge1
    let r = ping_with_retry(&edge2_stack, edge1_addr, 99).await;
    assert_eq!(r, 99);
}

/// Same as `edge_to_edge_through_router` but with a small Router<..., 4, 4>
/// to verify that `register_router` works with non-default const generics.
#[tokio::test]
async fn edge_to_edge_through_small_router() {
    let _ = env_logger::builder().is_test(true).try_init();

    type SmallRouter = Router<TokioStreamInterface, rand::rngs::StdRng, 4, 4>;
    type SmallRouterStack = ArcNetStack<CriticalSectionRawMutex, SmallRouter>;

    let router_stack: SmallRouterStack = SmallRouterStack::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);
    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

    // register_router infers Router<..., 4, 4> from the stack type
    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r1_read,
        r1_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_router(
        router_stack.clone(),
        r2_read,
        r2_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();

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

    spawn_ping_server(&edge1_stack);
    spawn_ping_server(&edge2_stack);

    let edge1_addr = Address {
        network_id: 1,
        node_id: 2,
        port_id: 0,
    };
    let edge2_addr = Address {
        network_id: 2,
        node_id: 2,
        port_id: 0,
    };

    ping_with_retry(&router_stack, edge1_addr, 0).await;
    ping_with_retry(&router_stack, edge2_addr, 0).await;

    wait_active(&edge1_stack).await;
    wait_active(&edge2_stack).await;

    // Edge1 → Edge2 through Router<..., 4, 4>
    let r = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(r, 42);
}
