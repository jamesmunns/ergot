/*
    Let's see, we're going to need:

    * Some kind of hashmap/vec of active interfaces, by network id?
        * IF we use a vec, we should NOT use the index as the ID, it may be sparse
    * The actual interface type probably gets defined by the interface manager
    * The interface which follows the pinned rules, and removes itself on drop
    * THIS version of the routing interface probably will not allow for other routers,
        we probably have to assume we are the only one assigning network IDs until a
        later point
    * The associated "simple" version of a client probably needs a stub routing interface
        that picks up the network ID from the destination address
    * Honestly we might want to have an `Arc` version of the netstack, or we need some kind
        of Once construction.
    * The interface manager needs some kind of "handle" construction so that we can get mut
        access to it, or we need an accessor via the netstack
*/

use std::sync::Arc;
use std::{cell::UnsafeCell, mem::MaybeUninit};

use crate::{
    Header, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError,
        cobs_stream::{self, Interface},
        std_utils::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    wire_frames::{CommonHeader, de_frame},
};

use bbq2::prod_cons::stream::StreamConsumer;
use bbq2::traits::storage::BoxedSlice;
use log::{debug, error, info, trace, warn};
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

pub struct StdTcpRecvHdl<R: ScopedRawMutex + 'static> {
    stack: &'static NetStack<R, StdTcpIm>,
    // TODO: when we have more real networking and we could possibly
    // have conflicting net_id assignments, we might need to have a
    // shared ref to an Arc<AtomicU16> or something for net_id?
    //
    // for now, stdtcp assumes it is the only "seed" router, meaning that
    // it is solely in charge of assigning netids
    net_id: u16,
    skt: OwnedReadHalf,
    closer: Arc<WaitQueue>,
}

pub struct StdTcpIm {
    init: bool,
    inner: UnsafeCell<MaybeUninit<StdTcpImInner>>,
}

#[derive(Default)]
pub struct StdTcpImInner {
    // TODO: we probably want something like iddqd for a hashset sorted by
    // net_id, as well as a list of "allocated" netids, mapped to the
    // interface they are associated with
    //
    // TODO: for the no-std version of this, we will need to use the same
    // intrusive list stuff that we use for sockets for holding interfaces.
    interfaces: Vec<StdTcpTxHdl>,
    seq_no: u16,
    any_closed: bool,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
}

struct StdTcpTxHdl {
    net_id: u16,
    skt_tx: Interface<StdQueue>,
    closer: Arc<WaitQueue>,
}

// ---- impls ----

// impl StdTcpRecvHdl

impl<R: ScopedRawMutex + 'static> StdTcpRecvHdl<R> {
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let res = self.run_inner().await;
        self.closer.close();
        // todo: this could live somewhere else?
        self.stack.with_interface_manager(|im| {
            let inner = im.get_or_init_inner();
            inner.any_closed = true;
        });
        res
    }

    pub async fn run_inner(&mut self) -> Result<(), ReceiverError> {
        let mut cobs_buf = CobsAccumulator::new(1024 * 1024);
        let mut raw_buf = [0u8; 4096];

        loop {
            let rd = self.skt.read(&mut raw_buf);
            let close = self.closer.wait();

            let ct = select! {
                r = rd => {
                    match r {
                        Ok(0) | Err(_) => {
                            warn!("recv run {} closed", self.net_id);
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
                            // If the message comes in and has a src net_id of zero,
                            // we should rewrite it so it isn't later understood as a
                            // local packet.
                            if frame.hdr.src.network_id == 0 {
                                assert_ne!(
                                    frame.hdr.src.node_id, 0,
                                    "we got a local packet remotely?"
                                );
                                assert_ne!(
                                    frame.hdr.src.node_id, 1,
                                    "someone is pretending to be us?"
                                );

                                frame.hdr.src.network_id = self.net_id;
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
                                    warn!("recv->send error: {e:?}");
                                }
                            }
                        } else {
                            warn!("Decode error! Ignoring frame on net_id {}", self.net_id);
                        }

                        remaining
                    }
                };
            }
        }
    }
}

// impl StdTcpIm

impl StdTcpIm {
    const fn new() -> Self {
        Self {
            init: false,
            inner: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub fn get_nets(&mut self) -> Vec<u16> {
        let inner = self.get_or_init_inner();
        inner.interfaces.iter().map(|i| i.net_id).collect()
    }

    fn get_or_init_inner(&mut self) -> &mut StdTcpImInner {
        let inner = self.inner.get_mut();
        if self.init {
            unsafe { inner.assume_init_mut() }
        } else {
            let imr = inner.write(StdTcpImInner::default());
            self.init = true;
            imr
        }
    }
}

impl StdTcpIm {
    fn common_send<'a, 'b>(
        &'b mut self,
        ihdr: &'a Header,
    ) -> Result<(&'b mut StdTcpTxHdl, CommonHeader), InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        assert!(!(ihdr.dst.port_id == 0 && ihdr.any_all.is_none()));

        let inner = self.get_or_init_inner();
        // todo: dedupe w/ send
        //
        // todo: we only handle direct dests
        let Ok(idx) = inner
            .interfaces
            .binary_search_by_key(&ihdr.dst.network_id, |int| int.net_id)
        else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        let interface = &mut inner.interfaces[idx];
        // TODO: Assumption: "we" are always node_id==1
        if ihdr.dst.network_id == interface.net_id && ihdr.dst.node_id == 1 {
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
            hdr.src.network_id = interface.net_id;
            hdr.src.node_id = 1;
        }

        let seq_no = inner.seq_no;
        inner.seq_no = inner.seq_no.wrapping_add(1);

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

        Ok((interface, header))
    }
}

impl InterfaceManager for StdTcpIm {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.skt_tx.send_ty(&header, hdr.any_all.as_ref(), data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.skt_tx.send_raw(&header, hdr_raw, data);

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
        let res = intfc.skt_tx.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}

impl Default for StdTcpIm {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstInit for StdTcpIm {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

unsafe impl Sync for StdTcpIm {}

// impl StdTcpImInner

impl StdTcpImInner {
    pub fn alloc_intfc(&mut self, tx: OwnedWriteHalf) -> Option<(u16, Arc<WaitQueue>)> {
        let closer = Arc::new(WaitQueue::new());
        if self.interfaces.is_empty() {
            // todo: configurable channel depth
            let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(4096));
            let ctx = q.stream_producer();
            let crx = q.stream_consumer();

            let ctx = cobs_stream::Interface {
                mtu: 1024,
                prod: ctx,
            };

            let net_id = 1;
            // TODO: We are spawning in a non-async context!
            tokio::task::spawn(tx_worker(net_id, tx, crx, closer.clone()));
            self.interfaces.push(StdTcpTxHdl {
                net_id,
                skt_tx: ctx,
                closer: closer.clone(),
            });
            debug!("Alloc'd net_id 1");
            return Some((net_id, closer));
        } else if self.interfaces.len() >= 65534 {
            warn!("Out of netids!");
            return None;
        }

        // If we closed any interfaces, then collect
        if self.any_closed {
            self.interfaces.retain(|int| {
                let closed = int.closer.is_closed();
                if closed {
                    info!("Collecting interface {}", int.net_id);
                }
                !closed
            });
        }

        let mut net_id = 1;
        // we're not empty, find the lowest free address by counting the
        // indexes, and if we find a discontinuity, allocate the first one.
        for intfc in self.interfaces.iter() {
            if intfc.net_id > net_id {
                trace!("Found gap: {net_id}");
                break;
            }
            debug_assert!(intfc.net_id == net_id);
            net_id += 1;
        }
        // EITHER: We've found a gap that we can use, OR we've iterated all
        // interfaces, which means that we had contiguous allocations but we
        // have not exhausted the range.
        debug_assert!(net_id > 0 && net_id != u16::MAX);

        let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(4096));
        let ctx = q.stream_producer();
        let crx = q.stream_consumer();

        let ctx = cobs_stream::Interface {
            mtu: 1024,
            prod: ctx,
        };

        debug!("allocated net_id {net_id}");

        tokio::task::spawn(tx_worker(net_id, tx, crx, closer.clone()));
        self.interfaces.push(StdTcpTxHdl {
            net_id,
            skt_tx: ctx,
            closer: closer.clone(),
        });
        self.interfaces.sort_unstable_by_key(|i| i.net_id);
        Some((net_id, closer))
    }
}

// Helper functions

async fn tx_worker(
    net_id: u16,
    mut tx: OwnedWriteHalf,
    rx: StreamConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
) {
    info!("Started tx_worker for net_id {net_id}");
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
        debug!("sending pkt len:{} on net_id {net_id}", len);
        let res = tx.write_all(&frame).await;
        frame.release(len);
        if let Err(e) = res {
            error!("Err: {e:?}");
            break;
        }
    }
    // TODO: GC waker?
    warn!("Closing interface {net_id}");
}

pub fn register_interface<R: ScopedRawMutex>(
    stack: &'static NetStack<R, StdTcpIm>,
    socket: TcpStream,
) -> Result<StdTcpRecvHdl<R>, Error> {
    let (rx, tx) = socket.into_split();
    stack.with_interface_manager(|im| {
        let inner = im.get_or_init_inner();
        if let Some((addr, closer)) = inner.alloc_intfc(tx) {
            Ok(StdTcpRecvHdl {
                stack,
                net_id: addr,
                skt: rx,
                closer,
            })
        } else {
            Err(Error::OutOfNetIds)
        }
    })
}
