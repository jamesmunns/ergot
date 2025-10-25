//! A std+tcp edge device profile
//!
//! This is useful for std based devices/applications that can directly connect to a DirectRouter
//! using a tcp connection.

use std::sync::Arc;

use crate::{
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::tokio_tcp::TokioTcpInterface,
        profiles::direct_edge::{DirectEdge, process_frame},
        utils::std::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    net_stack::NetStackHandle,
};

use crate::logging::{error, info, trace, warn};
use bbq2::{prod_cons::stream::StreamConsumer, traits::bbqhdl::BbqHandle};
use maitake_sync::WaitQueue;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    select,
};

pub type StdTcpClientIm = DirectEdge<TokioTcpInterface>;

pub struct RxWorker<N: NetStackHandle> {
    stack: N,
    skt: OwnedReadHalf,
    closer: Arc<WaitQueue>,
}

// ---- impls ----

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectEdge<TokioTcpInterface>>,
{
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let res = self.run_inner().await;
        // todo: this could live somewhere else?
        self.stack.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        });
        res
    }

    pub async fn run_inner(&mut self) -> Result<(), ReceiverError> {
        let mut cobs_buf = CobsAccumulator::new(1024 * 1024);
        let mut raw_buf = [0u8; 4096];
        let mut net_id = None;

        loop {
            let rd = self.skt.read(&mut raw_buf);
            let close = self.closer.wait();

            let ct = select! {
                r = rd => {
                    match r {
                        Ok(0) | Err(_) => {
                            warn!("recv run closed");
                            return Err(ReceiverError::SocketClosed)
                        },
                        Ok(ct) => ct,
                    }
                }
                _c = close => {
                    return Err(ReceiverError::SocketClosed);
                }
            };

            let buf = &mut raw_buf[..ct];
            let mut window = buf;

            'cobs: while !window.is_empty() {
                window = match cobs_buf.feed_raw(window) {
                    FeedResult::Consumed => break 'cobs,
                    FeedResult::OverFull(new_wind) => new_wind,
                    FeedResult::DecodeError(new_wind) => new_wind,
                    FeedResult::Success { data, remaining }
                    | FeedResult::SuccessInput { data, remaining } => {
                        // Successfully de-cobs'd a packet, now we need to
                        // do something with it.
                        process_frame(&mut net_id, data, &self.stack, ());

                        remaining
                    }
                };
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct SocketAlreadyActive;

// Helper functions

pub async fn register_target_interface<N>(
    stack: N,
    socket: TcpStream,
    queue: StdQueue,
) -> Result<(), SocketAlreadyActive>
where
    N: NetStackHandle<Profile = DirectEdge<TokioTcpInterface>>,
    N: Send + 'static,
{
    let (rx, tx) = socket.into_split();
    let closer = Arc::new(WaitQueue::new());
    stack.stack().manage_profile(|im| {
        match im.interface_state(()) {
            Some(InterfaceState::Down) => {}
            Some(InterfaceState::Inactive) => return Err(SocketAlreadyActive),
            Some(InterfaceState::ActiveLocal { .. }) => return Err(SocketAlreadyActive),
            Some(InterfaceState::Active { .. }) => return Err(SocketAlreadyActive),
            None => {}
        }

        im.set_interface_state((), InterfaceState::Inactive)
            .map_err(|_| SocketAlreadyActive)?;

        Ok(())
    })?;
    let rx_worker = RxWorker {
        stack,
        skt: rx,
        closer: closer.clone(),
    };
    // TODO: spawning in a non-async context!
    tokio::task::spawn(tx_worker(tx, queue.stream_consumer(), closer.clone()));
    tokio::task::spawn(rx_worker.run());
    Ok(())
}

async fn tx_worker(mut tx: OwnedWriteHalf, rx: StreamConsumer<StdQueue>, closer: Arc<WaitQueue>) {
    info!("Started tx_worker");
    loop {
        let rxf = rx.wait_read();
        let clf = closer.wait();

        let frame = select! {
            r = rxf => r,
            _c = clf => {
                break;
            }
        };

        let len = frame.len();
        trace!("sending pkt len:{}", len);
        let res = tx.write_all(&frame).await;
        frame.release(len);
        if let Err(e) = res {
            error!("Err: {:?}", e);
            break;
        }
    }
    // TODO: GC waker?
    warn!("Closing interface");
}
