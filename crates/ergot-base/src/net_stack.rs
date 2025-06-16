//! The Ergot NetStack
//!
//! The [`NetStack`] is the core of Ergot. It is intended to be placed
//! in a `static` variable for the duration of your application.
//!
//! The Netstack is used directly for a couple of main responsibilities:
//!
//! 1. Sending a message, either from user code, or to deliver/forward messages
//!    received from an interface
//! 2. Attaching a socket, allowing the NetStack to route messages to it
//! 3. Interacting with the [interface manager], in order to add/remove
//!    interfaces, or obtain other information
//!
//! [interface manager]: crate::interface_manager
//!
//! In general, interacting with anything contained by the [`NetStack`] requires
//! locking of the [`BlockingMutex`] which protects the inner contents. This
//! is used both to allow sharing of the inner contents, but also to allow
//! `Drop` impls to remove themselves from the stack in a blocking manner.

use core::{any::TypeId, mem::ManuallyDrop, pin::pin, ptr::NonNull};

use cordyceps::{Linked, List, list::Links};
use mutex::{BlockingMutex, ConstInit, ScopedRawMutex};
use postcard_rpc::Endpoint;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Address, FrameKind, Header,
    interface_manager::{self, InterfaceManager, InterfaceSendError},
    socket::{
        SocketHeader, SocketHeaderEndpointReq, SocketHeaderEndpointResp, SocketHeaderTopicIn,
        SocketSendError, SocketVTable, owned::OwnedSocket,
    },
};

/// The Ergot Netstack
pub struct NetStack<R: ScopedRawMutex, M: InterfaceManager> {
    inner: BlockingMutex<R, NetStackInner<M>>,
}

struct SocketLists {
    endpoint_reqs: List<SocketHeaderEndpointReq>,
    endpoint_resps: List<SocketHeaderEndpointResp>,
    topic_ins: List<SocketHeaderTopicIn>,
}

struct PortCache {
    pcache_bits: u32,
    pcache_start: u8,
}

pub(crate) struct NetStackInner<M: InterfaceManager> {
    sockets: SocketLists,
    manager: M,
    pcache: PortCache,
    seq_no: u16,
}

/// An error from calling a [`NetStack`] "send" method
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetStackSendError {
    SocketSend(SocketSendError),
    InterfaceSend(InterfaceSendError),
    NoRoute,
    AnyPortMissingKey,
    WrongPortKind,
}

// ---- impl NetStack ----

impl<R, M> NetStack<R, M>
where
    R: ScopedRawMutex + ConstInit,
    M: InterfaceManager + interface_manager::ConstInit,
{
    /// Create a new, uninitialized [`NetStack`].
    ///
    /// Requires that the [`ScopedRawMutex`] implements the [`mutex::ConstInit`]
    /// trait, and the [`InterfaceManager`] implements the
    /// [`interface_manager::ConstInit`] trait.
    ///
    /// ## Example
    ///
    /// ```rust
    /// use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// use ergot::NetStack;
    /// use ergot::interface_manager::null::NullInterfaceManager as NullIM;
    ///
    /// static STACK: NetStack<CSRMutex, NullIM> = NetStack::new();
    /// ```
    pub const fn new() -> Self {
        Self {
            inner: BlockingMutex::new(NetStackInner::new()),
        }
    }
}

impl<R, M> NetStack<R, M>
where
    R: ScopedRawMutex,
    M: InterfaceManager,
{
    /// Manually create a new, uninitialized [`NetStack`].
    ///
    /// This method is useful if your [`ScopedRawMutex`] or [`InterfaceManager`]
    /// do not implement their corresponding `ConstInit` trait.
    ///
    /// In general, this is most often only needed for `loom` testing, and
    /// [`NetStack::new()`] should be used when possible.
    pub const fn const_new(r: R, m: M) -> Self {
        Self {
            inner: BlockingMutex::const_new(
                r,
                NetStackInner {
                    sockets: SocketLists::new(),
                    manager: m,
                    seq_no: 0,
                    pcache: PortCache::new(),
                },
            ),
        }
    }

    /// Access the contained [`InterfaceManager`].
    ///
    /// Access to the [`InterfaceManager`] is made via the provided closure.
    /// The [`BlockingMutex`] is locked for the duration of this access,
    /// inhibiting all other usage of this [`NetStack`].
    ///
    /// This can be used to add new interfaces, obtain metadata, or other
    /// actions supported by the chosen [`InterfaceManager`].
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::null::NullInterfaceManager as NullIM;
    /// #
    /// static STACK: NetStack<CSRMutex, NullIM> = NetStack::new();
    ///
    /// let res = STACK.with_interface_manager(|im| {
    ///    // The mutex is locked for the full duration of this closure.
    ///    # _ = im;
    ///    // We can return whatever we want from this context, though not
    ///    // anything borrowed from `im`.
    ///    42
    /// });
    /// assert_eq!(res, 42);
    /// ```
    pub fn with_interface_manager<F: FnOnce(&mut M) -> U, U>(&'static self, f: F) -> U {
        self.inner.with_lock(|inner| f(&mut inner.manager))
    }

    /// Perform an [`Endpoint`] Request, and await Response.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::null::NullInterfaceManager as NullIM;
    /// use ergot::Address;
    /// // Define an example endpoint
    /// postcard_rpc::endpoint!(Example, u32, i32, "pathho");
    ///
    /// static STACK: NetStack<CSRMutex, NullIM> = NetStack::new();
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     // (not shown: starting an `Example` service...)
    ///     # let jhdl = tokio::task::spawn(async {
    ///     #     println!("Serve!");
    ///     #     let srv = ergot::socket::endpoint::OwnedEndpointSocket::<Example, _, _>::new(&STACK);
    ///     #     let srv = core::pin::pin!(srv);
    ///     #     let mut hdl = srv.attach();
    ///     #     hdl.serve(async |p| p as i32).await.unwrap();
    ///     #     println!("Served!");
    ///     # });
    ///     # // TODO: let the server attach first
    ///     # tokio::time::sleep(core::time::Duration::from_millis(10)).await;
    ///     // Make a ping request to local
    ///     let res = STACK.req_resp::<Example>(
    ///         Address::unknown(),
    ///         42u32,
    ///     ).await;
    ///     assert_eq!(res, Ok(42i32));
    ///     # jhdl.await.unwrap();
    /// }
    /// ```
    pub async fn req_resp<E>(
        &'static self,
        dst: Address,
        req: E::Request,
    ) -> Result<E::Response, NetStackSendError>
    where
        E: Endpoint,
        E::Request: Serialize + DeserializeOwned + 'static,
        E::Response: Serialize + DeserializeOwned + 'static,
    {
        let resp_sock = OwnedSocket::new_endpoint_resp::<E>(self);
        let resp_sock = pin!(resp_sock);
        let mut resp_hdl = resp_sock.attach();
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: resp_hdl.port(),
            },
            dst,
            key: Some(E::REQ_KEY),
            seq_no: None,
            kind: FrameKind::EndpointRequest,
        };
        self.send_ty(hdr, req)?;
        // TODO: assert seq nos match somewhere? do we NEED seq nos if we have
        // port ids now?
        let resp = resp_hdl.recv().await;
        Ok(resp.t)
    }

    /// Send a raw (pre-serialized) message.
    ///
    /// This interface should almost never be used by end-users, and is instead
    /// typically used by interfaces to feed received messages into the
    /// [`NetStack`].
    pub fn send_raw(&'static self, hdr: Header, body: &[u8]) -> Result<(), NetStackSendError> {
        if hdr.dst.port_id == 0 && hdr.key.is_none() {
            return Err(NetStackSendError::AnyPortMissingKey);
        }
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        self.inner
            .with_lock(|inner| inner.send_raw(local_bypass, hdr, body))
    }

    /// Send a typed message
    ///
    /// This is less spicy than `send_raw`, but will likely be deprecated in
    /// favor of easier-to-hold-right methods like [`Self::req_resp()`]. The
    /// provided `Key` MUST match the type `T`, e.g. [`Endpoint::REQ_KEY`],
    /// [`Endpoint::RESP_KEY`], or [`Topic::TOPIC_KEY`].
    ///
    /// [`Topic::TOPIC_KEY`]: postcard_rpc::Topic::TOPIC_KEY
    pub fn send_ty<T: 'static + Serialize>(
        &'static self,
        hdr: Header,
        t: T,
    ) -> Result<(), NetStackSendError> {
        // Can we assume the destination is local?
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        self.inner
            .with_lock(|inner| inner.send_ty(local_bypass, hdr, t))
    }

    pub(crate) unsafe fn try_attach_socket_edpt_req(
        &'static self,
        mut node: NonNull<SocketHeaderEndpointReq>,
    ) -> Option<u8> {
        self.inner.with_lock(|inner| {
            let new_port = inner.pcache.alloc_port(&inner.sockets)?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.endpoint_reqs.push_front(node);
            Some(new_port)
        })
    }

    pub(crate) unsafe fn try_attach_socket_edpt_resp(
        &'static self,
        mut node: NonNull<SocketHeaderEndpointResp>,
    ) -> Option<u8> {
        self.inner.with_lock(|inner| {
            let new_port = inner.pcache.alloc_port(&inner.sockets)?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.endpoint_resps.push_front(node);
            Some(new_port)
        })
    }

    pub(crate) unsafe fn try_attach_socket_tpc_in(
        &'static self,
        mut node: NonNull<SocketHeaderTopicIn>,
    ) -> Option<u8> {
        self.inner.with_lock(|inner| {
            let new_port = inner.pcache.alloc_port(&inner.sockets)?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.topic_ins.push_front(node);
            Some(new_port)
        })
    }

    pub(crate) unsafe fn detach_socket_edpt_req(
        &'static self,
        node: NonNull<SocketHeaderEndpointReq>,
    ) {
        self.inner.with_lock(|inner| unsafe {
            let port = node.as_ref().port;
            inner.pcache.free_port(port);
            inner.sockets.endpoint_reqs.remove(node)
        });
    }

    pub(crate) unsafe fn detach_socket_edpt_resp(
        &'static self,
        node: NonNull<SocketHeaderEndpointResp>,
    ) {
        self.inner.with_lock(|inner| unsafe {
            let port = node.as_ref().port;
            inner.pcache.free_port(port);
            inner.sockets.endpoint_resps.remove(node)
        });
    }

    pub(crate) unsafe fn detach_socket_tpc_in(&'static self, node: NonNull<SocketHeaderTopicIn>) {
        self.inner.with_lock(|inner| unsafe {
            let port = node.as_ref().port;
            inner.pcache.free_port(port);
            inner.sockets.topic_ins.remove(node)
        });
    }

    pub(crate) unsafe fn with_lock<U, F: FnOnce() -> U>(&'static self, f: F) -> U {
        self.inner.with_lock(|_inner| f())
    }
}

impl<R, M> Default for NetStack<R, M>
where
    R: ScopedRawMutex + ConstInit,
    M: InterfaceManager + interface_manager::ConstInit,
{
    fn default() -> Self {
        Self::new()
    }
}

// ---- impl SocketLists ----
impl SocketLists {
    pub const fn new() -> Self {
        Self {
            endpoint_reqs: List::new(),
            endpoint_resps: List::new(),
            topic_ins: List::new(),
        }
    }
}

impl Default for SocketLists {
    fn default() -> Self {
        Self::new()
    }
}

// ---- impl PortCache ----

impl PortCache {
    const fn new() -> Self {
        Self {
            pcache_bits: 0,
            pcache_start: 0,
        }
    }
}

impl Default for PortCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---- impl NetStackInner ----

impl<M> NetStackInner<M>
where
    M: InterfaceManager,
    M: interface_manager::ConstInit,
{
    pub const fn new() -> Self {
        Self {
            sockets: SocketLists::new(),
            manager: M::INIT,
            seq_no: 0,
            pcache: PortCache::new(),
        }
    }
}

impl<M> NetStackInner<M>
where
    M: InterfaceManager,
{
    fn send_raw(
        &mut self,
        local_bypass: bool,
        hdr: Header,
        body: &[u8],
    ) -> Result<(), NetStackSendError> {
        let res = if !local_bypass {
            self.manager.send_raw(hdr.clone(), body)
        } else {
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => return Ok(()),
            Err(InterfaceSendError::DestinationLocal) => {}
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        match hdr.kind {
            FrameKind::EndpointRequest => {
                send_raw_lcl(hdr, body, &mut self.seq_no, &mut self.sockets.endpoint_reqs)
            }
            FrameKind::EndpointResponse => send_raw_lcl(
                hdr,
                body,
                &mut self.seq_no,
                &mut self.sockets.endpoint_resps,
            ),
            FrameKind::Topic => {
                send_raw_lcl(hdr, body, &mut self.seq_no, &mut self.sockets.topic_ins)
            }
        }
    }

    fn send_ty<T: 'static + Serialize>(
        &mut self,
        local_bypass: bool,
        hdr: Header,
        t: T,
    ) -> Result<(), NetStackSendError> {
        let res = if !local_bypass {
            // Not local: offer to the interface manager to send
            self.manager.send(hdr.clone(), &t)
        } else {
            // just skip to local sending
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => return Ok(()),
            Err(InterfaceSendError::DestinationLocal) => {}
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        //
        // Sending to a local interface means a potential move. Create a
        // manuallydrop, if a send succeeds, then we have "moved from" here
        // into the destination. If no send succeeds (e.g. no socket match
        // or sending to the socket failed) then we will need to drop the
        // value ourselves.
        let mut t = ManuallyDrop::new(t);
        let that: NonNull<ManuallyDrop<T>> = NonNull::from(&mut t);
        let that: NonNull<()> = that.cast();
        let that_id = TypeId::of::<T>();

        let res = match hdr.kind {
            FrameKind::EndpointRequest => send_ty_lcl(
                hdr,
                that,
                &that_id,
                &mut self.seq_no,
                &mut self.sockets.endpoint_reqs,
            ),
            FrameKind::EndpointResponse => send_ty_lcl(
                hdr,
                that,
                &that_id,
                &mut self.seq_no,
                &mut self.sockets.endpoint_resps,
            ),
            FrameKind::Topic => send_ty_lcl(
                hdr,
                that,
                &that_id,
                &mut self.seq_no,
                &mut self.sockets.topic_ins,
            ),
        };

        // We reached the end of sockets. We need to drop this item.
        if res.is_err() {
            unsafe {
                ManuallyDrop::drop(&mut t);
            }
        }
        res
    }
}

fn send_raw_lcl<H: SocketHeader + Linked<Links<H>>>(
    hdr: Header,
    body: &[u8],
    seq_no: &mut u16,
    list: &mut List<H>,
) -> Result<(), NetStackSendError> {
    for socket in list.iter_raw() {
        let skt_ref = unsafe { socket.as_ref() };

        // TODO: only allow port_id == 0 if there is only one matching port
        // with this key.
        if (skt_ref.port() == hdr.dst.port_id)
            || (hdr.dst.port_id == 0 && hdr.key.is_some_and(|k| k == skt_ref.key()))
        {
            let res = {
                let f = skt_ref.vtable().send_raw;

                // SAFETY: skt_ref is now dead to us!

                let this: NonNull<H> = socket;
                let this: NonNull<()> = this.cast();
                let hdr = hdr.to_headerseq_or_with_seq(|| {
                    let seq = *seq_no;
                    *seq_no = seq_no.wrapping_add(1);
                    seq
                });

                (f)(this, body, hdr).map_err(NetStackSendError::SocketSend)
            };
            return res;
        }
    }
    Err(NetStackSendError::NoRoute)
}

fn send_ty_lcl<H: SocketHeader + Linked<Links<H>>>(
    hdr: Header,
    that: NonNull<()>,
    that_id: &TypeId,
    seq_no: &mut u16,
    list: &mut List<H>,
) -> Result<(), NetStackSendError> {
    // Check each socket to see if we want to send it there...
    for socket in list.iter_raw() {
        let skt_ref = unsafe { socket.as_ref() };

        // TODO: only allow port_id == 0 if there is only one matching port
        // with this key.
        if (skt_ref.port() == hdr.dst.port_id || hdr.dst.port_id == 0)
            && hdr.key.unwrap() == skt_ref.key()
        {
            let vtable: &'static SocketVTable = skt_ref.vtable();

            // SAFETY: skt_ref is now dead to us!

            let res = if let Some(f) = vtable.send_owned {
                let this: NonNull<H> = socket;
                let this: NonNull<()> = this.cast();
                let hdr = hdr.to_headerseq_or_with_seq(|| {
                    let seq = *seq_no;
                    *seq_no = seq_no.wrapping_add(1);
                    seq
                });
                (f)(this, that, hdr, that_id).map_err(NetStackSendError::SocketSend)
            } else if let Some(_f) = vtable.send_bor {
                // TODO: if we support send borrowed, then we need to
                // drop the manuallydrop here, success or failure.
                todo!()
            } else {
                // todo: keep going? If we found the "right" destination and
                // sending fails, then there's not much we can do. Probably: there
                // is no case where a socket has NEITHER send_owned NOR send_bor,
                // can we make this state impossible instead?
                Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
            };

            return res;
        }
    }
    Err(NetStackSendError::NoRoute)
}

impl PortCache {
    /// Cache-based allocator inspired by littlefs2 ID allocator
    ///
    /// We remember 32 ports at a time, from the current base, which is always
    /// a multiple of 32. Allocating from this range does not require moving thru
    /// the socket lists.
    ///
    /// If the current 32 ports are all taken, we will start over from a base port
    /// of 0, and attempt to
    fn alloc_port(&mut self, sockets: &SocketLists) -> Option<u8> {
        // ports 0 is always taken (could be clear on first alloc)
        self.pcache_bits |= (self.pcache_start == 0) as u32;

        if self.pcache_bits != u32::MAX {
            // We can allocate from the current slot
            let ldg = self.pcache_bits.trailing_ones();
            debug_assert!(ldg < 32);
            self.pcache_bits |= 1 << ldg;
            return Some(self.pcache_start + (ldg as u8));
        }

        // Nope, cache is all taken. try to find a base with available items.
        // We always start from the bottom to keep ports small, but if we know
        // we just exhausted a range, don't waste time checking that
        let old_start = self.pcache_start;
        for base in 0..8 {
            let start = base * 32;
            if start == old_start {
                continue;
            }
            // Clear/reset cache
            self.pcache_start = start;
            self.pcache_bits = 0;
            // port 0 is not allowed
            self.pcache_bits |= (self.pcache_start == 0) as u32;
            // port 255 is not allowed
            self.pcache_bits |= ((self.pcache_start == 0b111_00000) as u32) << 31;

            // TODO: If we trust that sockets are always sorted, we could early-return
            // when we reach a `pupper > self.pcache_start`. We could also maybe be smart
            // and iterate forwards for 0..4 and backwards for 4..8 (and switch the early
            // return check to < instead). NOTE: We currently do NOT guarantee sockets are
            // sorted!
            let ereqs = sockets.endpoint_reqs.iter().map(|n| n.port);
            let eresps = sockets.endpoint_resps.iter().map(|n| n.port);
            let tpins = sockets.topic_ins.iter().map(|n| n.port);
            let iter = ereqs.chain(eresps).chain(tpins);
            iter.for_each(|port| {
                // The upper 3 bits of the port
                let pupper = port & !(32 - 1);
                // The lower 5 bits of the port
                let plower = port & (32 - 1);

                if pupper == self.pcache_start {
                    self.pcache_bits |= 1 << plower;
                }
            });

            if self.pcache_bits != u32::MAX {
                // We can allocate from the current slot
                let ldg = self.pcache_bits.trailing_ones();
                debug_assert!(ldg < 32);
                self.pcache_bits |= 1 << ldg;
                return Some(self.pcache_start + (ldg as u8));
            }
        }

        // Nope, nothing found
        None
    }

    fn free_port(&mut self, port: u8) {
        // The upper 3 bits of the port
        let pupper = port & !(32 - 1);
        // The lower 5 bits of the port
        let plower = port & (32 - 1);

        // TODO: If the freed port is in the 0..32 range, or just less than
        // the current start range, maybe do an opportunistic re-look?
        if pupper == self.pcache_start {
            self.pcache_bits &= !(1 << plower);
        }
    }
}

#[cfg(test)]
mod test {
    use core::pin::pin;
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use postcard_rpc::topic;
    use std::thread::JoinHandle;
    use tokio::sync::oneshot;

    use crate::{
        NetStack, interface_manager::null::NullInterfaceManager, socket::owned::OwnedSocket,
    };

    #[test]
    fn port_alloc() {
        static STACK: NetStack<CriticalSectionRawMutex, NullInterfaceManager> = NetStack::new();
        topic!(FakeTopic, u32, "lol");

        let mut v = vec![];

        fn spawn_skt(id: u8) -> (u8, JoinHandle<()>, oneshot::Sender<()>) {
            let (txdone, rxdone) = oneshot::channel();
            let (txwait, rxwait) = oneshot::channel();
            let hdl = std::thread::spawn(move || {
                let skt = OwnedSocket::new_topic_in::<FakeTopic>(&STACK);
                let skt = pin!(skt);
                let hdl = skt.attach();
                assert_eq!(hdl.port(), id);
                txwait.send(()).unwrap();
                let _: () = rxdone.blocking_recv().unwrap();
            });
            let _ = rxwait.blocking_recv();
            (id, hdl, txdone)
        }

        // make sockets 1..32
        for i in 1..32 {
            v.push(spawn_skt(i));
        }

        // make sockets 32..40
        for i in 32..40 {
            v.push(spawn_skt(i));
        }

        // drop socket 35
        let pos = v.iter().position(|(i, _, _)| *i == 35).unwrap();
        let (_i, hdl, tx) = v.remove(pos);
        tx.send(()).unwrap();
        hdl.join().unwrap();

        // make a new socket, it should be 35
        v.push(spawn_skt(35));

        // drop socket 4
        let pos = v.iter().position(|(i, _, _)| *i == 4).unwrap();
        let (_i, hdl, tx) = v.remove(pos);
        tx.send(()).unwrap();
        hdl.join().unwrap();

        // make a new socket, it should be 40
        v.push(spawn_skt(40));

        // make sockets 41..64
        for i in 41..64 {
            v.push(spawn_skt(i));
        }

        // make a new socket, it should be 4
        v.push(spawn_skt(4));

        // make sockets 64..255
        for i in 64..255 {
            v.push(spawn_skt(i));
        }

        // drop socket 212
        let pos = v.iter().position(|(i, _, _)| *i == 212).unwrap();
        let (_i, hdl, tx) = v.remove(pos);
        tx.send(()).unwrap();
        hdl.join().unwrap();

        // make a new socket, it should be 212
        v.push(spawn_skt(212));

        // Sockets exhausted (we never see 255)
        let hdl = std::thread::spawn(move || {
            let skt = OwnedSocket::new_topic_in::<FakeTopic>(&STACK);
            let skt = pin!(skt);
            let hdl = skt.attach();
            println!("{}", hdl.port());
        });
        assert!(hdl.join().is_err());

        for (_i, hdl, tx) in v.drain(..) {
            tx.send(()).unwrap();
            hdl.join().unwrap();
        }
    }
}
