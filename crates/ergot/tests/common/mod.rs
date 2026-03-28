//! Shared helpers for E2E integration tests.

use std::{pin::pin, time::Duration};

use ergot::{
    Address,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::direct_edge::DirectEdge,
        utils::{cobs_stream, std::new_std_queue},
    },
    net_stack::{ArcNetStack, NetStackHandle},
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::time::{sleep, timeout};

pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

pub fn make_edge_stack() -> (EdgeStack, ergot::interface_manager::utils::std::StdQueue) {
    let queue = new_std_queue(4096);
    let stack = EdgeStack::new_with_profile(DirectEdge::new_target(
        cobs_stream::Sink::new_from_handle(queue.clone(), 512),
    ));
    (stack, queue)
}

pub fn spawn_ping_server(stack: &EdgeStack) {
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

pub async fn ping_with_retry<N: NetStackHandle + Clone>(stack: &N, addr: Address, val: u32) -> u32 {
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

#[allow(dead_code)]
pub async fn wait_active(stack: &EdgeStack) {
    for _ in 0..50 {
        let state = stack.manage_profile(|im| im.interface_state(()));
        if matches!(state, Some(InterfaceState::Active { .. })) {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("edge never reached Active state");
}
