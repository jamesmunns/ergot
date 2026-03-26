//! End-to-end tests: two DirectEdge stacks communicating through a DirectRouter.
//!
//! Uses `tokio::io::duplex()` to create in-memory bidirectional pipes.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

mod common;

use common::{make_edge_stack, ping_with_retry, spawn_ping_server, wait_active};
use ergot::{
    Address,
    interface_manager::{
        InterfaceState, interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::EdgeFrameProcessor, profiles::direct_router::DirectRouter,
        profiles::router::Router, transports::tokio_cobs_stream,
    },
    net_stack::ArcNetStack,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;

type RouterStackTy = ArcNetStack<CriticalSectionRawMutex, DirectRouter<TokioStreamInterface>>;

#[tokio::test]
async fn edge_to_edge_through_router() {
    //  Edge1 <--duplex1--> Router <--duplex2--> Edge2
    let _ = env_logger::builder().is_test(true).try_init();

    let router_stack: RouterStackTy = RouterStackTy::new();
    let (edge1_stack, edge1_queue) = make_edge_stack();
    let (edge2_stack, edge2_queue) = make_edge_stack();

    let (e1_read, r1_write) = tokio::io::duplex(8192);
    let (r1_read, e1_write) = tokio::io::duplex(8192);
    let (e2_read, r2_write) = tokio::io::duplex(8192);
    let (r2_read, e2_write) = tokio::io::duplex(8192);

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

    let response = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(response, 42);

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

    let r = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(r, 42);

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

    let r = ping_with_retry(&edge1_stack, edge2_addr, 42).await;
    assert_eq!(r, 42);
}
