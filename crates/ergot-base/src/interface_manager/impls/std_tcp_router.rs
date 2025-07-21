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

use crate::interface_manager::utils::edge::CentralInterface;
use crate::{
    Header, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError,
        utils::cobs_stream::{self, Interface},
        utils::std::{
            ReceiverError, StdQueue,
            acc::{CobsAccumulator, FeedResult},
        },
    },
    wire_frames::de_frame,
};

use bbq2::{prod_cons::stream::StreamConsumer, traits::storage::BoxedSlice};
use cobs::max_encoding_overhead;
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
    mtu: u16,
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
    interfaces: Vec<Node>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
}

pub struct Node {
    interface: CentralInterface<Interface<StdQueue>>,
}

// ---- impls ----

impl Node {
    pub fn new(
        net_id: u16,
        outgoing_buffer_size: usize,
        max_ergot_packet_size: u16,
    ) -> (Self, StreamConsumer<StdQueue>) {
        // todo: configurable channel depth
        let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(outgoing_buffer_size));
        let ctx = q.stream_producer();
        let crx = q.stream_consumer();

        let ctx = cobs_stream::Interface {
            mtu: max_ergot_packet_size,
            prod: ctx,
        };

        let me = Node {
            interface: CentralInterface::new(ctx, net_id),
        };

        (me, crx)
    }
}

// impl StdTcpRecvHdl

impl<R: ScopedRawMutex + 'static> StdTcpRecvHdl<R> {
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let close = self.closer.clone();

        // Wait for the receiver to encounter an error, or wait for
        // the transmitter to signal that it observed an error
        let res = select! {
            run = self.run_inner() => {
                // Halt the TX worker
                self.closer.close();
                Err(run)
            },
            _clf = close.wait() => Err(ReceiverError::SocketClosed),
        };

        // Remove this interface from the list
        self.stack.with_interface_manager(|im| {
            let inner = im.get_or_init_inner();
            inner
                .interfaces
                .retain(|n| n.interface.net_id() != self.net_id);
        });
        res
    }

    pub async fn run_inner(&mut self) -> ReceiverError {
        let overhead = max_encoding_overhead(self.mtu as usize);
        let mut cobs_buf = CobsAccumulator::new(self.mtu as usize + overhead);
        let mut raw_buf = vec![0u8; 4096].into_boxed_slice();

        loop {
            let rd = self.skt.read(&mut raw_buf);
            let close = self.closer.wait();

            let ct = select! {
                r = rd => {
                    match r {
                        Ok(0) | Err(_) => {
                            warn!("recv run {} closed", self.net_id);
                            return ReceiverError::SocketClosed
                        },
                        Ok(ct) => ct,
                    }
                }
                _c = close => {
                    return ReceiverError::SocketClosed;
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
        inner
            .interfaces
            .iter()
            .map(|i| i.interface.net_id())
            .collect()
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
    fn find<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<&'b mut CentralInterface<Interface<StdQueue>>, InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        assert!(!(ihdr.dst.port_id == 0 && ihdr.any_all.is_none()));

        let inner = self.get_or_init_inner();

        // todo: dedupe w/ send
        //
        // todo: we only handle direct dests
        let Ok(idx) = inner
            .interfaces
            .binary_search_by_key(&ihdr.dst.network_id, |int| int.interface.net_id())
        else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        Ok(&mut inner.interfaces[idx].interface)
    }
}

impl InterfaceManager for StdTcpIm {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send(hdr, data)
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send_raw(hdr, hdr_raw, data)
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send_err(hdr, err)
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
    pub fn alloc_intfc(
        &mut self,
        tx: OwnedWriteHalf,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Option<(u16, Arc<WaitQueue>)> {
        let closer = Arc::new(WaitQueue::new());
        if self.interfaces.is_empty() {
            let net_id = 1;
            let (node, crx) = Node::new(net_id, outgoing_buffer_size, max_ergot_packet_size);
            // TODO: We are spawning in a non-async context!
            tokio::task::spawn(tx_worker(net_id, tx, crx, closer.clone()));
            self.interfaces.push(node);
            debug!("Alloc'd net_id 1");
            return Some((net_id, closer));
        } else if self.interfaces.len() >= 65534 {
            warn!("Out of netids!");
            return None;
        }

        let mut net_id = 1;
        // we're not empty, find the lowest free address by counting the
        // indexes, and if we find a discontinuity, allocate the first one.
        for intfc in self.interfaces.iter() {
            if intfc.interface.net_id() > net_id {
                trace!("Found gap: {net_id}");
                break;
            }
            debug_assert!(intfc.interface.net_id() == net_id);
            net_id += 1;
        }
        // EITHER: We've found a gap that we can use, OR we've iterated all
        // interfaces, which means that we had contiguous allocations but we
        // have not exhausted the range.
        debug_assert!(net_id > 0 && net_id != u16::MAX);

        let (node, crx) = Node::new(net_id, outgoing_buffer_size, max_ergot_packet_size);
        debug!("allocated net_id {net_id}");

        tokio::task::spawn(tx_worker(net_id, tx, crx, closer.clone()));
        self.interfaces.push(node);
        self.interfaces
            .sort_unstable_by_key(|i| i.interface.net_id());
        Some((net_id, closer))
    }
}

// Helper functions

async fn tx_worker(
    net_id: u16,
    tx: OwnedWriteHalf,
    rx: StreamConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
) {
    tx_worker_inner(net_id, tx, rx, &closer).await;
    warn!("Closing interface {net_id}");
    closer.close();
}

async fn tx_worker_inner(
    net_id: u16,
    mut tx: OwnedWriteHalf,
    rx: StreamConsumer<StdQueue>,
    closer: &WaitQueue,
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
}

pub fn register_interface<R: ScopedRawMutex>(
    stack: &'static NetStack<R, StdTcpIm>,
    socket: TcpStream,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
) -> Result<StdTcpRecvHdl<R>, Error> {
    let (rx, tx) = socket.into_split();
    stack.with_interface_manager(|im| {
        let inner = im.get_or_init_inner();
        if let Some((addr, closer)) =
            inner.alloc_intfc(tx, max_ergot_packet_size, outgoing_buffer_size)
        {
            Ok(StdTcpRecvHdl {
                stack,
                net_id: addr,
                skt: rx,
                closer,
                mtu: max_ergot_packet_size,
            })
        } else {
            Err(Error::OutOfNetIds)
        }
    })
}
