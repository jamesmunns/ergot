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

#[allow(dead_code)]
pub type EdgeStack = ArcNetStack<CriticalSectionRawMutex, DirectEdge<TokioStreamInterface>>;

#[allow(dead_code)]
pub fn make_edge_stack() -> (EdgeStack, ergot::interface_manager::utils::std::StdQueue) {
    let queue = new_std_queue(4096);
    let stack = EdgeStack::new_with_profile(DirectEdge::new_target(
        cobs_stream::Sink::new_from_handle(queue.clone(), 512),
    ));
    (stack, queue)
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Bus mock: simulates a shared medium (ESP-NOW, CAN FD, RS-485)
// ---------------------------------------------------------------------------

/// A simulated shared bus where all taps hear all frames except their own.
///
/// Uses `tokio::sync::broadcast` internally. Each [`BusTap`] can send
/// complete frames and receive frames sent by other taps on the same bus.
#[allow(dead_code)]
pub struct Bus {
    sender: tokio::sync::broadcast::Sender<(usize, Vec<u8>)>,
    next_id: std::sync::atomic::AtomicUsize,
}

/// One device's connection to a [`Bus`].
///
/// - `send()`: broadcast a frame to all other taps
/// - `recv()`: receive the next frame from any other tap
#[allow(dead_code)]
pub struct BusTap {
    pub id: usize,
    sender: tokio::sync::broadcast::Sender<(usize, Vec<u8>)>,
    receiver: tokio::sync::broadcast::Receiver<(usize, Vec<u8>)>,
}

#[allow(dead_code)]
impl Bus {
    pub fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(256);
        Self {
            sender,
            next_id: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Create a new tap on this bus. Each tap gets a unique ID.
    pub fn tap(&self) -> BusTap {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BusTap {
            id,
            sender: self.sender.clone(),
            receiver: self.sender.subscribe(),
        }
    }
}

#[allow(dead_code)]
impl BusTap {
    /// Broadcast a complete frame to all other taps.
    pub fn send(&self, data: &[u8]) {
        let _ = self.sender.send((self.id, data.to_vec()));
    }

    /// Receive the next frame from any other tap (skips own frames).
    pub async fn recv(&mut self) -> Vec<u8> {
        loop {
            match self.receiver.recv().await {
                Ok((sender_id, data)) if sender_id != self.id => return data,
                Ok(_) => continue,      // own frame, skip
                Err(_) => return vec![], // channel closed
            }
        }
    }
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
