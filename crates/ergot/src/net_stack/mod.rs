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

use core::{fmt::Arguments, ops::Deref, ptr::NonNull};

use cordyceps::{List, list::Iter};
use endpoints::Endpoints;
use mutex::{BlockingMutex, ConstInit, ScopedRawMutex};
use serde::Serialize;
use topics::Topics;

use crate::{
    Header, ProtocolError,
    fmtlog::{ErgotFmtTx, Level},
    interface_manager::{self, InterfaceSendError, Profile},
    socket::{SocketHeader, SocketSendError},
    well_known::ErgotFmtTxTopic,
};

#[cfg(feature = "std")]
pub mod arc;
mod inner;
pub mod services;

#[cfg(feature = "std")]
pub use arc::ArcNetStack;
use inner::NetStackInner;
pub use services::Services;
pub mod discovery;
pub mod endpoints;
pub mod topics;

/// The Ergot Netstack
pub struct NetStack<R: ScopedRawMutex, P: Profile> {
    inner: BlockingMutex<R, NetStackInner<P>>,
}

pub trait NetStackHandle
where
    Self: Sized + Clone,
{
    type Target: Deref<Target = NetStack<Self::Mutex, Self::Profile>> + Clone;
    type Mutex: ScopedRawMutex;
    type Profile: Profile;
    fn stack(&self) -> Self::Target;
}

/// An error from calling a [`NetStack`] "send" method
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
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
    WouldDeadlock,
}

// ---- impl NetStack ----

impl<R, P> NetStackHandle for &'_ NetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    type Mutex = R;
    type Profile = P;
    type Target = Self;

    fn stack(&self) -> Self::Target {
        self
    }
}

impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex + ConstInit,
    P: Profile + interface_manager::ConstInit,
{
    /// Create a new, uninitialized [`NetStack`].
    ///
    /// Requires that the [`ScopedRawMutex`] implements the [`mutex::ConstInit`]
    /// trait, and the [`Profile`] implements the
    /// [`interface_manager::ConstInit`] trait.
    ///
    /// ## Example
    ///
    /// ```rust
    /// use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// use ergot::NetStack;
    /// use ergot::interface_manager::profiles::null::Null;
    ///
    /// static STACK: NetStack<CSRMutex, Null> = NetStack::new();
    /// ```
    pub const fn new() -> Self {
        Self {
            inner: BlockingMutex::new(NetStackInner::new()),
        }
    }
}

impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex + ConstInit,
    P: Profile,
{
    pub const fn new_with_profile(p: P) -> Self {
        Self {
            inner: BlockingMutex::new(NetStackInner::new_with_profile(p)),
        }
    }
}

#[cfg(feature = "std")]
impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex + ConstInit,
    P: Profile,
{
    pub(crate) fn new_arc(p: P) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner: BlockingMutex::new(NetStackInner::new_with_profile(p)),
        })
    }
}
impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    /// Manually create a new, uninitialized [`NetStack`].
    ///
    /// This method is useful if your [`ScopedRawMutex`] or [`Profile`]
    /// do not implement their corresponding `ConstInit` trait.
    ///
    /// In general, this is most often only needed for `loom` testing, and
    /// [`NetStack::new()`] should be used when possible.
    pub const fn const_new(r: R, p: P) -> Self {
        Self {
            inner: BlockingMutex::const_new(
                r,
                NetStackInner {
                    sockets: List::new(),
                    profile: p,
                    seq_no: 0,
                    pcache_start: 0,
                    pcache_bits: 0,
                },
            ),
        }
    }

    /// Access the contained [`Profile`].
    ///
    /// Access to the [`Profile`] is made via the provided closure.
    /// The [`BlockingMutex`] is locked for the duration of this access,
    /// inhibiting all other usage of this [`NetStack`].
    ///
    /// This can be used to add new interfaces, obtain metadata, or other
    /// actions supported by the chosen [`Profile`].
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::profiles::null::Null;
    /// #
    /// static STACK: NetStack<CSRMutex, Null> = NetStack::new();
    ///
    /// let res = STACK.manage_profile(|im| {
    ///    // The mutex is locked for the full duration of this closure.
    ///    # _ = im;
    ///    // We can return whatever we want from this context, though not
    ///    // anything borrowed from `im`.
    ///    42
    /// });
    /// assert_eq!(res, 42);
    /// ```
    pub fn manage_profile<F: FnOnce(&mut P) -> U, U>(&self, f: F) -> U {
        self.inner.with_lock(|inner| f(&mut inner.profile))
    }

    /// Send a raw (pre-serialized) message.
    ///
    /// This interface should almost never be used by end-users, and is instead
    /// typically used by interfaces to feed received messages into the
    /// [`NetStack`].
    pub fn send_raw(
        &self,
        hdr: &Header,
        hdr_raw: &[u8],
        body: &[u8],
        source: P::InterfaceIdent,
    ) -> Result<(), NetStackSendError> {
        self.inner
            .try_with_lock(|inner| inner.send_raw(hdr, hdr_raw, body, source))
            .ok_or(NetStackSendError::WouldDeadlock)?
    }

    /// Send a typed message
    pub fn send_ty<T: 'static + Serialize + Clone>(
        &self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner
            .try_with_lock(|inner| inner.send_ty(hdr, t))
            .ok_or(NetStackSendError::WouldDeadlock)?
    }

    pub fn send_bor<T: Serialize>(&self, hdr: &Header, t: &T) -> Result<(), NetStackSendError> {
        self.inner
            .try_with_lock(|inner| inner.send_bor(hdr, t))
            .ok_or(NetStackSendError::WouldDeadlock)?
    }

    pub fn send_err(
        &self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<P::InterfaceIdent>,
    ) -> Result<(), NetStackSendError> {
        self.inner
            .try_with_lock(|inner| inner.send_err(hdr, err, source))
            .ok_or(NetStackSendError::WouldDeadlock)?
    }

    /// Call the given function with an iterator over all discoverable sockets
    ///
    /// Returns None if the mutex is already locked.
    pub fn with_sockets<F, U>(&self, f: F) -> Option<U>
    where
        for<'b> F: FnOnce(SocketHeaderIter<'b>) -> U,
    {
        self.inner
            .try_with_lock(|inner| inner.with_sockets::<F, U>(f))
    }

    pub(crate) unsafe fn try_attach_socket(&self, mut node: NonNull<SocketHeader>) -> Option<u8> {
        self.inner.try_with_lock(|inner| {
            let new_port = inner.alloc_port()?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.push_front(node);
            Some(new_port)
        })?
    }

    pub(crate) unsafe fn attach_broadcast_socket(&self, mut node: NonNull<SocketHeader>) {
        self.inner.with_lock(|inner| {
            unsafe {
                node.as_mut().port = 255;
            }
            inner.sockets.push_back(node);
        });
    }

    pub(crate) unsafe fn attach_socket(&self, node: NonNull<SocketHeader>) -> u8 {
        let res = unsafe { self.try_attach_socket(node) };
        let Some(new_port) = res else {
            panic!("exhausted all addrs");
        };
        new_port
    }

    pub(crate) unsafe fn detach_socket(&self, node: NonNull<SocketHeader>) {
        self.inner.with_lock(|inner| unsafe {
            let port = node.as_ref().port;
            if port != 255 {
                inner.free_port(port);
            }
            inner.sockets.remove(node)
        });
    }

    pub(crate) unsafe fn with_lock<U, F: FnOnce() -> U>(&self, f: F) -> U {
        self.inner.with_lock(|_inner| f())
    }
}

/// An iterator over all discoverable [`SocketHeader`]s
///
/// NOTE: this interface does NOT give access to the sockets in a way that allows for
/// type punning/inner mutability. ONLY usable for querying socket header information.
pub struct SocketHeaderIter<'a> {
    pub(crate) iter: Iter<'a, SocketHeader>,
}

impl<'a> Iterator for SocketHeaderIter<'a> {
    type Item = &'a SocketHeader;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let skt = self.iter.next()?;

            // Only yield discoverable sockets
            if skt.attrs.discoverable {
                return Some(skt);
            }
        }
    }
}

impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    /// Send a trace-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn trace_fmt(&self, args: &Arguments<'_>) {
        self.level_fmt(Level::Trace, args);
    }

    /// Send a debug-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn debug_fmt(&self, args: &Arguments<'_>) {
        self.level_fmt(Level::Debug, args);
    }

    /// Send an info-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn info_fmt(&self, args: &Arguments<'_>) {
        self.level_fmt(Level::Info, args);
    }

    /// Send a warn-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn warn_fmt(&self, args: &Arguments<'_>) {
        self.level_fmt(Level::Warn, args);
    }

    /// Send an error-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn error_fmt(&self, args: &Arguments<'_>) {
        self.level_fmt(Level::Error, args);
    }

    fn level_fmt(&self, level: Level, args: &Arguments<'_>) {
        _ = self
            .topics()
            .broadcast_borrowed::<ErgotFmtTxTopic>(&ErgotFmtTx { level, inner: args }, None);
    }

    pub fn services(&self) -> Services<&Self> {
        Services { inner: self }
    }

    pub fn endpoints(&self) -> Endpoints<&Self> {
        Endpoints { inner: self }
    }

    pub fn topics(&self) -> Topics<&Self> {
        Topics { inner: self }
    }
}

#[derive(Debug, PartialEq)]
pub enum ReqRespError {
    // An error occurred locally while sending
    Local(NetStackSendError),
    // An error occurred remotely while waiting for response
    Remote(ProtocolError),
    // Requests cannot be sent to broadcast ports
    NoBroadcast,
}

impl<R, P> Default for NetStack<R, P>
where
    R: ScopedRawMutex + ConstInit,
    P: Profile + interface_manager::ConstInit,
{
    fn default() -> Self {
        Self::new()
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
            NetStackSendError::WouldDeadlock => ProtocolError::NSSE_WOULD_DEADLOCK,
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
        interface_manager::profiles::null::Null,
        socket::{Attributes, owned::single::Socket},
    };

    #[test]
    fn port_alloc() {
        static STACK: NetStack<CriticalSectionRawMutex, Null> = NetStack::new();

        let mut v = vec![];

        fn spawn_skt(id: u8) -> (u8, JoinHandle<()>, oneshot::Sender<()>) {
            let (txdone, rxdone) = oneshot::channel();
            let (txwait, rxwait) = oneshot::channel();
            let hdl = std::thread::spawn(move || {
                let skt = Socket::<u64, &NetStack<CriticalSectionRawMutex, Null>>::new(
                    &STACK,
                    Key(*b"TEST1234"),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                    None,
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
            let skt = Socket::<u64, &NetStack<CriticalSectionRawMutex, Null>>::new(
                &STACK,
                Key(*b"TEST1234"),
                Attributes {
                    kind: FrameKind::ENDPOINT_REQ,
                    discoverable: true,
                },
                None,
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
