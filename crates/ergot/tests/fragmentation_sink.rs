//! Tests for the fragmentation sink

#![cfg(feature = "tokio-std")]

use std::{assert_matches, pin::pin, time::Duration};

use ergot::{
    endpoint,
    interface_manager::{
        InterfaceState,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::{CENTRAL_NODE_ID, DirectEdge, EDGE_NODE_ID, EdgeFrameProcessor},
        transports::tokio_cobs_stream,
        utils::{
            cobs_stream,
            fragmentation_sink::{
                DefaultFragmentationIssueHandler, FragmentationSinkBuilder,
                FragmentationSinkInterface, calc_barray_size, calc_registry_size,
            },
            std::StdQueue,
        },
    },
    net_stack::ArcNetStack,
    toolkits::tokio_stream::{self as stream_kit},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::{
    select,
    time::{sleep, timeout},
};

const INNER_MTU: usize = 64;
const MTU: usize = 1024;
const DEVICE_ADDR: ergot::Address = ergot::Address {
    network_id: 1,
    node_id: 2,
    port_id: 0,
};

type FragStack = ArcNetStack<
    CriticalSectionRawMutex,
    DirectEdge<FragmentationSinkInterface<TokioStreamInterface, StdQueue>>,
>;

type LargeVec = heapless::Vec<u8, 255>;

endpoint!(NormalSizedEndpoint, u64, u64, "normal-sized");
endpoint!(OversizedEndpoint, LargeVec, LargeVec, "over-sized");

/// Spawn a ping server on the target stack.
fn spawn_ping_server(stack: &FragStack) {
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

/// Spawn a ping server on the target stack.
fn spawn_normal_sized_server(stack: &FragStack) {
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<NormalSizedEndpoint, 4>(None);
            let server = pin!(server);
            let mut hdl = server.attach();
            loop {
                let _ = hdl
                    .serve(|val: &u64| {
                        let v = *val;
                        async move { v }
                    })
                    .await;
            }
        }
    });
}

/// Spawn a ping server on the target stack.
fn spawn_oversized_server(stack: &FragStack) {
    tokio::spawn({
        let stack = stack.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<OversizedEndpoint, 4>(None);
            let server = pin!(server);
            let mut hdl = server.attach();
            loop {
                let _ = hdl
                    .serve(|val: &LargeVec| {
                        let v = val.to_owned();
                        async move { v }
                    })
                    .await;
            }
        }
    });
}

/// Send a ping with retries (the first ping establishes the target's net_id).
async fn ping_with_retry(stack: &FragStack, val: u32) -> u32 {
    for _ in 0..20 {
        let result = timeout(
            Duration::from_millis(500),
            stack
                .endpoints()
                .request::<ErgotPingEndpoint>(DEVICE_ADDR, &val, None),
        )
        .await;
        match result {
            Ok(Ok(v)) => return v,
            _ => sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("ping failed after retries");
}

#[tokio::test]
async fn fragmentation_test() {
    env_logger::init();
    const SIZE: usize = calc_barray_size::<TokioStreamInterface, StdQueue, INNER_MTU>();
    const REGISTRY_SIZE: usize = calc_registry_size::<MTU, SIZE>();

    let (ctrl_read, tgt_write) = tokio::io::duplex(8192);
    let (tgt_read, ctrl_write) = tokio::io::duplex(8192);

    let ctrl_queue = stream_kit::new_std_queue(4096);
    let frag_queue = stream_kit::new_std_queue(4096);
    let mut frag_sink_builder =
        FragmentationSinkBuilder::<_, _, _, MTU, 1, SIZE, REGISTRY_SIZE>::new(
            DefaultFragmentationIssueHandler,
        );
    frag_sink_builder.with_bbqueue(Some(frag_queue));
    frag_sink_builder.with_sink(cobs_stream::Sink::new_from_handle(
        ctrl_queue.clone(),
        INNER_MTU as u16,
    ));

    let (sink, ctrl_config) = frag_sink_builder.generate();

    let ctrl_stack: FragStack =
        ArcNetStack::new_with_profile(DirectEdge::new_controller(sink, InterfaceState::Down));

    let tgt_queue = stream_kit::new_std_queue(4096);
    let tgt_frag_queue = stream_kit::new_std_queue(4096);
    let mut tgt_frag_sink_builder =
        FragmentationSinkBuilder::<_, _, _, MTU, 1, SIZE, REGISTRY_SIZE>::new(
            DefaultFragmentationIssueHandler,
        );
    tgt_frag_sink_builder.with_bbqueue(Some(tgt_frag_queue));
    tgt_frag_sink_builder.with_sink(cobs_stream::Sink::new_from_handle(
        tgt_queue.clone(),
        INNER_MTU as u16,
    ));

    let (sink, tgt_config) = tgt_frag_sink_builder.generate();
    let tgt_stack: FragStack = ArcNetStack::new_with_profile(DirectEdge::new_target(sink));

    tokio_cobs_stream::register_edge::<
        _,
        FragmentationSinkInterface<TokioStreamInterface, StdQueue>,
        _,
        _,
    >(
        ctrl_stack.clone(),
        ctrl_read,
        ctrl_write,
        ctrl_queue,
        EdgeFrameProcessor::new_controller(1),
        InterfaceState::Active {
            net_id: 1,
            node_id: CENTRAL_NODE_ID,
        },
        None,
        None,
    )
    .await
    .unwrap();

    tokio_cobs_stream::register_edge::<
        _,
        FragmentationSinkInterface<TokioStreamInterface, StdQueue>,
        _,
        _,
    >(
        tgt_stack.clone(),
        tgt_read,
        tgt_write,
        tgt_queue,
        EdgeFrameProcessor::new(),
        InterfaceState::Active {
            net_id: 1,
            node_id: EDGE_NODE_ID,
        },
        None,
        None,
    )
    .await
    .unwrap();

    spawn_ping_server(&tgt_stack.clone());
    spawn_normal_sized_server(&tgt_stack.clone());
    spawn_oversized_server(&tgt_stack.clone());

    select!(
        _ = ctrl_stack
            .services()
            .fragmented_message_handler::<StdQueue, DirectEdge<FragmentationSinkInterface<TokioStreamInterface, StdQueue>>, _, 4, MTU, 1, SIZE, REGISTRY_SIZE>(ctrl_config, ()) => {}
        _ = tgt_stack
            .services()
            .fragmented_message_handler::<StdQueue, DirectEdge<FragmentationSinkInterface<TokioStreamInterface, StdQueue>>, _, 4, MTU, 1, SIZE, REGISTRY_SIZE>(tgt_config, ()) => {}
        _ = async {

            // Run a ping to make sure the target has the correct net_id
            let result = ping_with_retry(&ctrl_stack, 42).await;

            assert_matches!(result, 42);

            let result = ctrl_stack.endpoints().request::<NormalSizedEndpoint>(DEVICE_ADDR, &u64::MAX, None).await;

            assert_matches!(result, Ok(u64::MAX));

            let _vec: LargeVec = heapless::Vec::from_iter(std::iter::repeat_n(5, 255));

            let result = ctrl_stack.endpoints().request::<OversizedEndpoint>(DEVICE_ADDR, &_vec, None).await;
            assert_matches!(result, Ok(_vec));

        } => {}
    );
}
