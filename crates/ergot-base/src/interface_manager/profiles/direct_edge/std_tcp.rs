//! A std+tcp edge device profile
//!
//! This is useful for std based devices/applications that can directly connect to a DirectRouter
//! using a tcp connection.

use std::sync::Arc;

use crate::{
    Header,
    interface_manager::{
        InterfaceState, Profile,
        interface_impls::std_tcp::StdTcpInterface,
        profiles::direct_edge::{DirectEdge, EDGE_NODE_ID},
        utils::std::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

use bbq2::{prod_cons::stream::StreamConsumer, traits::bbqhdl::BbqHandle};
use log::{debug, error, info, warn};
use maitake_sync::WaitQueue;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    select,
};

pub type StdTcpClientIm = DirectEdge<StdTcpInterface>;

pub struct RxWorker<N: NetStackHandle> {
    stack: N,
    skt: OwnedReadHalf,
    closer: Arc<WaitQueue>,
}

// ---- impls ----

impl<N> RxWorker<N>
where
    N: NetStackHandle<Profile = DirectEdge<StdTcpInterface>>,
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

            let buf = &raw_buf[..ct];
            let mut window = buf;

            'cobs: while !window.is_empty() {
                window = match cobs_buf.feed_raw(window) {
                    FeedResult::Consumed => break 'cobs,
                    FeedResult::OverFull(new_wind) => new_wind,
                    FeedResult::DeserError(new_wind) => new_wind,
                    FeedResult::Success { data, remaining } => {
                        // Successfully de-cobs'd a packet, now we need to
                        // do something with it.
                        if let Some(mut frame) = de_frame(data) {
                            debug!("Got Frame!");
                            let take_net = net_id.is_none()
                                || net_id.is_some_and(|n| {
                                    frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id
                                });
                            if take_net {
                                self.stack.stack().manage_profile(|im| {
                                    im.set_interface_state(
                                        (),
                                        InterfaceState::Active {
                                            net_id: frame.hdr.dst.network_id,
                                            node_id: EDGE_NODE_ID,
                                        },
                                    )
                                    .unwrap();
                                });
                                net_id = Some(frame.hdr.dst.network_id);
                            }

                            // If the message comes in and has a src net_id of zero,
                            // we should rewrite it so it isn't later understood as a
                            // local packet.
                            //
                            // TODO: accept any packet if we don't have a net_id yet?
                            if let Some(net) = net_id.as_ref() {
                                if frame.hdr.src.network_id == 0 {
                                    assert_ne!(
                                        frame.hdr.src.node_id, 0,
                                        "we got a local packet remotely?"
                                    );
                                    assert_ne!(
                                        frame.hdr.src.node_id, 2,
                                        "someone is pretending to be us?"
                                    );

                                    frame.hdr.src.network_id = *net;
                                }
                            }

                            // TODO: if the destination IS self.net_id, we could rewrite the
                            // dest net_id as zero to avoid a pass through the interface manager.
                            //
                            // If the dest is 0, should we rewrite the dest as self.net_id? This
                            // is the opposite as above, but I dunno how that will work with responses
                            let hdr = frame.hdr.clone();
                            let hdr: Header = hdr.into();
                            let res = match frame.body {
                                Ok(body) => self.stack.stack().send_raw(&hdr, frame.hdr_raw, body),
                                Err(e) => self.stack.stack().send_err(&hdr, e),
                            };
                            match res {
                                Ok(()) => {}
                                Err(e) => {
                                    // TODO: match on error, potentially try to send NAK?
                                    panic!("recv->send error: {e:?}");
                                }
                            }
                        } else {
                            warn!(
                                "Decode error! Ignoring frame on net_id {}",
                                net_id.unwrap_or(0)
                            );
                        }

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
    N: NetStackHandle<Profile = DirectEdge<StdTcpInterface>>,
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
        info!("sending pkt len:{}", len);
        let res = tx.write_all(&frame).await;
        frame.release(len);
        if let Err(e) = res {
            error!("Err: {e:?}");
            break;
        }
    }
    // TODO: GC waker?
    warn!("Closing interface");
}
