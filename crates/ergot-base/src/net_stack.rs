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

use core::{any::TypeId, ptr::NonNull};

use cordyceps::List;
use log::{debug, trace};
use mutex::{BlockingMutex, ConstInit, ScopedRawMutex};
use serde::Serialize;

use crate::{
    FrameKind, Header, ProtocolError,
    interface_manager::{self, InterfaceManager, InterfaceSendError},
    socket::{SocketHeader, SocketSendError, SocketVTable},
};

/// The Ergot Netstack
pub struct NetStack<R: ScopedRawMutex, M: InterfaceManager> {
    inner: BlockingMutex<R, NetStackInner<M>>,
}

pub(crate) struct NetStackInner<M: InterfaceManager> {
    sockets: List<SocketHeader>,
    manager: M,
    pcache_bits: u32,
    pcache_start: u8,
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
    AnyPortNotUnique,
    AllPortMissingKey,
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
    /// use ergot_base::NetStack;
    /// use ergot_base::interface_manager::null::NullInterfaceManager as NullIM;
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
                    sockets: List::new(),
                    manager: m,
                    seq_no: 0,
                    pcache_start: 0,
                    pcache_bits: 0,
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
    /// # use ergot_base::NetStack;
    /// # use ergot_base::interface_manager::null::NullInterfaceManager as NullIM;
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

    /// Send a raw (pre-serialized) message.
    ///
    /// This interface should almost never be used by end-users, and is instead
    /// typically used by interfaces to feed received messages into the
    /// [`NetStack`].
    pub fn send_raw(&'static self, hdr: &Header, body: &[u8]) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_raw(hdr, body))
    }

    /// Send a typed message
    pub fn send_ty<T: 'static + Serialize + Clone>(
        &'static self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_ty(hdr, t))
    }

    pub fn send_err(
        &'static self,
        hdr: &Header,
        err: ProtocolError,
    ) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_err(hdr, err))
    }

    pub(crate) unsafe fn try_attach_socket(
        &'static self,
        mut node: NonNull<SocketHeader>,
    ) -> Option<u8> {
        self.inner.with_lock(|inner| {
            let new_port = inner.alloc_port()?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.push_front(node);
            Some(new_port)
        })
    }

    pub(crate) unsafe fn attach_broadcast_socket(&'static self, mut node: NonNull<SocketHeader>) {
        self.inner.with_lock(|inner| {
            unsafe {
                node.as_mut().port = 255;
            }
            inner.sockets.push_back(node);
        });
    }

    pub(crate) unsafe fn attach_socket(&'static self, node: NonNull<SocketHeader>) -> u8 {
        let res = unsafe { self.try_attach_socket(node) };
        let Some(new_port) = res else {
            panic!("exhausted all addrs");
        };
        new_port
    }

    pub(crate) unsafe fn detach_socket(&'static self, node: NonNull<SocketHeader>) {
        self.inner.with_lock(|inner| unsafe {
            let port = node.as_ref().port;
            if port != 255 {
                inner.free_port(port);
            }
            inner.sockets.remove(node)
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

// ---- impl NetStackInner ----

impl<M> NetStackInner<M>
where
    M: InterfaceManager,
    M: interface_manager::ConstInit,
{
    pub const fn new() -> Self {
        Self {
            sockets: List::new(),
            manager: M::INIT,
            seq_no: 0,
            pcache_bits: 0,
            pcache_start: 0,
        }
    }
}

impl<M> NetStackInner<M>
where
    M: InterfaceManager,
{
    /// Method that handles broadcast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn broadcast<SendSocket, SendMgr>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        mut sskt: SendSocket,
        smgr: SendMgr,
    ) -> Result<(), NetStackSendError>
    where
        SendSocket: FnMut(NonNull<SocketHeader>) -> bool,
        SendMgr: FnOnce() -> bool,
    {
        trace!("Sending msg broadcast w/ header: {hdr:?}");
        let res_lcl = {
            let bcast_iter = Self::find_all_local(sockets, hdr)?;
            let mut any_found = false;
            for dst in bcast_iter {
                let res = sskt(dst);
                if res {
                    debug!("delivered broadcast message locally");
                }
                any_found |= res;
            }
            any_found
        };

        let res_rmt = smgr();
        if res_rmt {
            debug!("delivered broadcast message remotely");
        }

        if res_lcl || res_rmt {
            Ok(())
        } else {
            Err(NetStackSendError::NoRoute)
        }
    }

    /// Method that handles unicast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn unicast<SendSocket, SendMgr>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        sskt: SendSocket,
        smgr: SendMgr,
    ) -> Result<(), NetStackSendError>
    where
        SendSocket: FnOnce(NonNull<SocketHeader>) -> Result<(), NetStackSendError>,
        SendMgr: FnOnce() -> Result<(), InterfaceSendError>,
    {
        trace!("Sending msg unicast w/ header: {hdr:?}");
        // Can we assume the destination is local?
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        let res = if !local_bypass {
            // Not local: offer to the interface manager to send
            debug!("Offering msg externally unicast w/ header: {hdr:?}");
            smgr()
        } else {
            // just skip to local sending
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => {
                debug!("Externally routed msg unicast");
                return Ok(());
            }
            Err(InterfaceSendError::DestinationLocal) => {
                debug!("No external interest in msg unicast");
            }
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        let socket = if hdr.dst.port_id == 0 {
            debug!("Sending ANY unicast msg locally w/ header: {hdr:?}");
            Self::find_any_local(sockets, hdr)
        } else {
            debug!("Sending ONE unicast msg locally w/ header: {hdr:?}");
            Self::find_one_local(sockets, hdr)
        }?;

        sskt(socket)
    }

    /// Method that handles unicast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn unicast_err<SendSocket, SendMgr>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        sskt: SendSocket,
        smgr: SendMgr,
    ) -> Result<(), NetStackSendError>
    where
        SendSocket: FnOnce(NonNull<SocketHeader>) -> Result<(), NetStackSendError>,
        SendMgr: FnOnce() -> Result<(), InterfaceSendError>,
    {
        trace!("Sending err unicast w/ header: {hdr:?}");
        // Can we assume the destination is local?
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        let res = if !local_bypass {
            // Not local: offer to the interface manager to send
            debug!("Offering err externally unicast w/ header: {hdr:?}");
            smgr()
        } else {
            // just skip to local sending
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => {
                debug!("Externally routed err unicast");
                return Ok(());
            }
            Err(InterfaceSendError::DestinationLocal) => {
                debug!("No external interest in err unicast");
            }
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        let socket = Self::find_one_err_local(sockets, hdr)?;

        sskt(socket)
    }

    /// Handle sending of a raw (serialized) message
    fn send_raw(&mut self, hdr: &Header, body: &[u8]) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            manager,
            ..
        } = self;
        trace!("Sending msg raw w/ header: {hdr:?}");

        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        if hdr.dst.port_id == 255 {
            Self::broadcast(
                sockets,
                hdr,
                |skt| Self::send_raw_to_socket(skt, body, hdr, seq_no).is_ok(),
                || manager.send_raw(hdr, body).is_ok(),
            )
        } else {
            Self::unicast(
                sockets,
                hdr,
                |skt| Self::send_raw_to_socket(skt, body, hdr, seq_no),
                || manager.send_raw(hdr, body),
            )
        }
    }

    /// Handle sending of a typed message
    fn send_ty<T: 'static + Serialize + Clone>(
        &mut self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            manager,
            ..
        } = self;
        trace!("Sending msg ty w/ header: {hdr:?}");

        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        if hdr.dst.port_id == 255 {
            Self::broadcast(
                sockets,
                hdr,
                |skt| Self::send_ty_to_socket(skt, t, hdr, seq_no).is_ok(),
                || manager.send(hdr, t).is_ok(),
            )
        } else {
            Self::unicast(
                sockets,
                hdr,
                |skt| Self::send_ty_to_socket(skt, t, hdr, seq_no),
                || manager.send(hdr, t),
            )
        }
    }

    /// Handle sending of a typed message
    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            manager,
            ..
        } = self;
        trace!("Sending msg ty w/ header: {hdr:?}");

        if hdr.dst.port_id == 255 {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        Self::unicast_err(
            sockets,
            hdr,
            |skt| Self::send_err_to_socket(skt, err, hdr, seq_no),
            || manager.send_err(hdr, err),
        )
    }

    /// Find a specific (e.g. port_id not 0 or 255) destination port matching
    /// the given header.
    fn find_one_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find the specific matching port
        let mut iter = sockets.iter_raw();
        let socket = loop {
            let Some(skt) = iter.next() else {
                return Err(NetStackSendError::NoRoute);
            };
            let skt_ref = unsafe { skt.as_ref() };
            if skt_ref.port != hdr.dst.port_id {
                continue;
            }
            if skt_ref.attrs.kind != hdr.kind {
                return Err(NetStackSendError::WrongPortKind);
            }
            break skt;
        };
        Ok(socket)
    }

    /// Find a specific (e.g. port_id not 0 or 255) destination port matching
    /// the given header.
    fn find_one_err_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find the specific matching port
        let mut iter = sockets.iter_raw();
        let socket = loop {
            let Some(skt) = iter.next() else {
                return Err(NetStackSendError::NoRoute);
            };
            let skt_ref = unsafe { skt.as_ref() };
            if skt_ref.port != hdr.dst.port_id {
                continue;
            }
            break skt;
        };
        Ok(socket)
    }

    /// Find a wildcard (e.g. port_id == 0) destination port matching the given header.
    ///
    /// If more than one port matches the wildcard, an error is returned.
    /// Does not match sockets that does not have the `discoverable` [`Attributes`].
    fn find_any_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find ONE specific matching port
        let Some(key) = hdr.key.as_ref() else {
            return Err(NetStackSendError::AnyPortMissingKey);
        };
        let mut iter = sockets.iter_raw();
        let mut socket: Option<NonNull<SocketHeader>> = None;

        loop {
            let Some(skt) = iter.next() else {
                break;
            };
            let skt_ref = unsafe { skt.as_ref() };

            // Check for things that would disqualify a socket from being an
            // "ANY" destination
            let mut illegal = false;
            illegal |= skt_ref.attrs.kind != hdr.kind;
            illegal |= !skt_ref.attrs.discoverable;
            illegal |= &skt_ref.key != key;

            if illegal {
                // Wait, that's illegal
                continue;
            }

            // It's a match! Is it a second match?
            if socket.is_some() {
                return Err(NetStackSendError::AnyPortNotUnique);
            }
            // Nope! Store this one, then we keep going to ensure that no
            // other socket matches this description.
            socket = Some(skt);
        }

        socket.ok_or(NetStackSendError::NoRoute)
    }

    /// Find ALL broadcast (e.g. port_id == 255) sockets matching the given header.
    ///
    /// Returns an error if the header does not contain a Key. May return zero
    /// matches.
    fn find_all_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<impl Iterator<Item = NonNull<SocketHeader>>, NetStackSendError> {
        let Some(key) = hdr.key.as_ref() else {
            return Err(NetStackSendError::AllPortMissingKey);
        };
        Ok(sockets.iter_raw().filter(move |socket| {
            let skt_ref = unsafe { socket.as_ref() };
            let bport = skt_ref.port == 255;
            let dkind = skt_ref.attrs.kind == hdr.kind;
            let dkey = &skt_ref.key == key;
            bport && dkind && dkey
        }))
    }

    /// Helper method for sending a type to a given socket
    fn send_ty_to_socket<T: 'static + Serialize + Clone>(
        this: NonNull<SocketHeader>,
        t: &T,
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };

        if let Some(f) = vtable.recv_owned {
            let this: NonNull<()> = this.cast();
            let that: NonNull<T> = NonNull::from(t);
            let that: NonNull<()> = that.cast();
            let hdr = hdr.to_headerseq_or_with_seq(|| {
                let seq = *seq_no;
                *seq_no = seq_no.wrapping_add(1);
                seq
            });
            (f)(this, that, hdr, &TypeId::of::<T>()).map_err(NetStackSendError::SocketSend)
        } else if let Some(_f) = vtable.recv_bor {
            // TODO: support send borrowed
            todo!()
        } else {
            // todo: keep going? If we found the "right" destination and
            // sending fails, then there's not much we can do. Probably: there
            // is no case where a socket has NEITHER send_owned NOR send_bor,
            // can we make this state impossible instead?
            Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
        }
    }

    /// Helper method for sending a type to a given socket
    fn send_err_to_socket(
        this: NonNull<SocketHeader>,
        err: ProtocolError,
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };

        if let Some(f) = vtable.recv_err {
            let this: NonNull<()> = this.cast();
            let hdr = hdr.to_headerseq_or_with_seq(|| {
                let seq = *seq_no;
                *seq_no = seq_no.wrapping_add(1);
                seq
            });
            (f)(this, hdr, err);
            Ok(())
        } else {
            // todo: keep going? If we found the "right" destination and
            // sending fails, then there's not much we can do. Probably: there
            // is no case where a socket has NEITHER send_owned NOR send_bor,
            // can we make this state impossible instead?
            Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
        }
    }

    // /// Helper message for sending a raw message to a given socket
    // fn send_err_raw_to_socket(
    //     this: NonNull<SocketHeader>,
    //     body: &[u8],
    //     hdr: &Header,
    //     seq_no: &mut u16,
    // ) -> Result<(), NetStackSendError> {
    //     let vtable: &'static SocketVTable = {
    //         let skt_ref = unsafe { this.as_ref() };
    //         skt_ref.vtable
    //     };
    //     let f = vtable.recv_raw;

    //     let this: NonNull<()> = this.cast();
    //     let hdr = hdr.to_headerseq_or_with_seq(|| {
    //         let seq = *seq_no;
    //         *seq_no = seq_no.wrapping_add(1);
    //         seq
    //     });

    //     (f)(this, body, hdr).map_err(NetStackSendError::SocketSend)
    // }

    /// Helper message for sending a raw message to a given socket
    fn send_raw_to_socket(
        this: NonNull<SocketHeader>,
        body: &[u8],
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };
        let f = vtable.recv_raw;

        let this: NonNull<()> = this.cast();
        let hdr = hdr.to_headerseq_or_with_seq(|| {
            let seq = *seq_no;
            *seq_no = seq_no.wrapping_add(1);
            seq
        });

        (f)(this, body, hdr).map_err(NetStackSendError::SocketSend)
    }
}

impl<M> NetStackInner<M>
where
    M: InterfaceManager,
{
    /// Cache-based allocator inspired by littlefs2 ID allocator
    ///
    /// We remember 32 ports at a time, from the current base, which is always
    /// a multiple of 32. Allocating from this range does not require moving thru
    /// the socket lists.
    ///
    /// If the current 32 ports are all taken, we will start over from a base port
    /// of 0, and attempt to
    fn alloc_port(&mut self) -> Option<u8> {
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
            self.sockets.iter().for_each(|s| {
                if s.port == 255 {
                    return;
                }

                // The upper 3 bits of the port
                let pupper = s.port & !(32 - 1);
                // The lower 5 bits of the port
                let plower = s.port & (32 - 1);

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
        debug_assert!(port != 255);
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

impl NetStackSendError {
    pub fn to_error(&self) -> ProtocolError {
        match self {
            NetStackSendError::SocketSend(socket_send_error) => socket_send_error.to_error(),
            NetStackSendError::InterfaceSend(interface_send_error) => {
                interface_send_error.to_error()
            }
            NetStackSendError::NoRoute => ProtocolError::NSSE_NO_ROUTE,
            NetStackSendError::AnyPortMissingKey => ProtocolError::NSSE_ANY_PORT_MISSING_KEY,
            NetStackSendError::WrongPortKind => ProtocolError::NSSE_WRONG_PORT_KIND,
            NetStackSendError::AnyPortNotUnique => ProtocolError::NSSE_ANY_PORT_NOT_UNIQUE,
            NetStackSendError::AllPortMissingKey => ProtocolError::NSSE_ALL_PORT_MISSING_KEY,
        }
    }
}

#[cfg(test)]
mod test {
    use core::pin::pin;
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use std::thread::JoinHandle;
    use tokio::sync::oneshot;

    use crate::{
        FrameKind, Key, NetStack,
        interface_manager::null::NullInterfaceManager,
        socket::{Attributes, owned::OwnedSocket},
    };

    #[test]
    fn port_alloc() {
        static STACK: NetStack<CriticalSectionRawMutex, NullInterfaceManager> = NetStack::new();

        let mut v = vec![];

        fn spawn_skt(id: u8) -> (u8, JoinHandle<()>, oneshot::Sender<()>) {
            let (txdone, rxdone) = oneshot::channel();
            let (txwait, rxwait) = oneshot::channel();
            let hdl = std::thread::spawn(move || {
                let skt = OwnedSocket::<u64, _, _>::new(
                    &STACK,
                    Key(*b"TEST1234"),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                );
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
            let skt = OwnedSocket::<u64, _, _>::new(
                &STACK,
                Key(*b"TEST1234"),
                Attributes {
                    kind: FrameKind::ENDPOINT_REQ,
                    discoverable: true,
                },
            );
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
