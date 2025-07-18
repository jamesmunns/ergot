// I need an interface manager that can have 0 or 1 interfaces
// it needs to be able to be const init'd (empty)
// at runtime we can attach the client (and maybe re-attach?)
//
// In normal setups, we'd probably want some way to "announce" we
// are here, but in point-to-point

use std::sync::Arc;

use crate::{
    Header, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError, cobs_stream,
        std_utils::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    wire_frames::{CommonHeader, de_frame},
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

#[derive(Default)]
pub struct StdTcpClientIm {
    inner: Option<StdTcpClientImInner>,
    seq_no: u16,
}

struct StdTcpClientImInner {
    interface: StdTcpTxHdl,
    net_id: u16,
    closer: Arc<WaitQueue>,
}

#[derive(Debug, PartialEq)]
pub enum ClientError {
    SocketAlreadyActive,
}

pub struct StdTcpRecvHdl<R: ScopedRawMutex + 'static> {
    stack: &'static NetStack<R, StdTcpClientIm>,
    skt: OwnedReadHalf,
    closer: Arc<WaitQueue>,
}

struct StdTcpTxHdl {
    skt_tx: cobs_stream::Interface<StdQueue>,
}

// ---- impls ----

impl StdTcpClientIm {
    pub const fn new() -> Self {
        Self {
            inner: None,
            seq_no: 0,
        }
    }
}

impl ConstInit for StdTcpClientIm {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

impl StdTcpClientIm {
    fn common_send<'a, 'b>(
        &'b mut self,
        ihdr: &'a Header,
    ) -> Result<(&'b mut StdTcpClientImInner, CommonHeader), InterfaceSendError> {
        let intfc = match self.inner.take() {
            None => return Err(InterfaceSendError::NoRouteToDest),
            Some(intfc) if intfc.closer.is_closed() => {
                drop(intfc);
                return Err(InterfaceSendError::NoRouteToDest);
            }
            Some(intfc) => self.inner.insert(intfc),
        };

        if intfc.net_id == 0 {
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        //
        // TODO: Assumption: "we" are always node_id==2
        if ihdr.dst.network_id == intfc.net_id && ihdr.dst.node_id == 2 {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Now that we've filtered out "dest local" checks, see if there is
        // any TTL left before we send to the next hop
        let mut hdr = ihdr.clone();
        hdr.decrement_ttl()?;

        // If the source is local, rewrite the source using this interface's
        // information so responses can find their way back here
        if hdr.src.net_node_any() {
            // todo: if we know the destination is EXACTLY this network,
            // we could leave the network_id local to allow for shorter
            // addresses
            hdr.src.network_id = intfc.net_id;
            hdr.src.node_id = 2;
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = intfc.net_id;
            hdr.dst.node_id = 1;
        }

        let seq_no = self.seq_no;
        self.seq_no = self.seq_no.wrapping_add(1);

        let header = CommonHeader {
            src: hdr.src,
            dst: hdr.dst,
            seq_no,
            kind: hdr.kind,
            ttl: hdr.ttl,
        };
        if [0, 255].contains(&hdr.dst.port_id) {
            if ihdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
        }

        Ok((intfc, header))
    }
}

impl InterfaceManager for StdTcpClientIm {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc
            .interface
            .skt_tx
            .send_ty(&header, hdr.any_all.as_ref(), data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        raw_hdr: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_raw(&header, raw_hdr, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}

impl<R: ScopedRawMutex + 'static> StdTcpRecvHdl<R> {
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let res = self.run_inner().await;
        // todo: this could live somewhere else?
        self.stack.with_interface_manager(|im| {
            _ = im.inner.take();
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
                                    if let Some(i) = im.inner.as_mut() {
                                        // i am, whoever you say i am
                                        i.net_id = frame.hdr.dst.network_id;
                                    }
                                    // else: uhhhhhh
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
        if im.inner.is_some() {
            return Err(ClientError::SocketAlreadyActive);
        }

        let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(4096));
        let ctx = q.stream_producer();
        let crx = q.stream_consumer();

        im.inner = Some(StdTcpClientImInner {
            interface: StdTcpTxHdl {
                skt_tx: cobs_stream::Interface {
                    mtu: 1024,
                    prod: ctx,
                },
            },
            net_id: 0,
            closer: closer.clone(),
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
