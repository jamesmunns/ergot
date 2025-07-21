// I need an interface manager that can have 0 or 1 interfaces
// it needs to be able to be const init'd (empty)
// at runtime we can attach the client (and maybe re-attach?)
//
// In normal setups, we'd probably want some way to "announce" we
// are here, but in point-to-point

use std::sync::Arc;

use crate::{
    Header, NetStack,
    interface_manager::utils::{
        cobs_stream,
        edge::EdgeInterface,
        std::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    wire_frames::de_frame,
};

use bbq2::{prod_cons::stream::StreamConsumer, traits::storage::BoxedSlice};
use log::{debug, error, info, warn};
use maitake_sync::WaitQueue;
use mutex::ScopedRawMutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    select,
};

pub type StdTcpClientIm = EdgeInterface<cobs_stream::Interface<StdQueue>>;

#[derive(Debug, PartialEq)]
pub enum ClientError {
    SocketAlreadyActive,
}

pub struct StdTcpRecvHdl<R: ScopedRawMutex + 'static> {
    stack: &'static NetStack<R, StdTcpClientIm>,
    skt: OwnedReadHalf,
    closer: Arc<WaitQueue>,
}

// ---- impls ----

impl<R: ScopedRawMutex + 'static> StdTcpRecvHdl<R> {
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let res = self.run_inner().await;
        // todo: this could live somewhere else?
        self.stack.with_interface_manager(|im| {
            _ = im.deregister();
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
                                self.stack.with_interface_manager(|im| {
                                    _ = im.set_net_id(frame.hdr.dst.network_id);
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
                                Ok(body) => self.stack.send_raw(&hdr, frame.hdr_raw, body),
                                Err(e) => self.stack.send_err(&hdr, e),
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

// Helper functions

pub fn register_interface<R: ScopedRawMutex>(
    stack: &'static NetStack<R, StdTcpClientIm>,
    socket: TcpStream,
) -> Result<StdTcpRecvHdl<R>, ClientError> {
    let (rx, tx) = socket.into_split();
    let closer = Arc::new(WaitQueue::new());
    stack.with_interface_manager(|im| {
        if im.is_active() {
            return Err(ClientError::SocketAlreadyActive);
        }

        let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(4096));
        let ctx = q.stream_producer();
        let crx = q.stream_consumer();

        im.register(cobs_stream::Interface {
            mtu: 1024,
            prod: ctx,
        });

        // TODO: spawning in a non-async context!
        tokio::task::spawn(tx_worker(tx, crx, closer.clone()));
        Ok(())
    })?;
    Ok(StdTcpRecvHdl {
        stack,
        skt: rx,
        closer,
    })
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
