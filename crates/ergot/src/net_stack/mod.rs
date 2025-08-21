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

use core::{fmt::Arguments, ops::Deref, pin::pin, ptr::NonNull};

use cordyceps::List;
use mutex::{BlockingMutex, ConstInit, ScopedRawMutex};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, ProtocolError,
    fmtlog::{ErgotFmtTx, Level},
    interface_manager::{self, InterfaceSendError, Profile},
    nash::NameHash,
    socket::{HeaderMessage, SocketHeader, SocketSendError},
    traits::{Endpoint, Topic},
    well_known::ErgotFmtTxTopic,
};

#[cfg(feature = "tokio-std")]
pub mod arc;
mod inner;
pub mod services;

#[cfg(feature = "tokio-std")]
pub use arc::ArcNetStack;
use inner::NetStackInner;
pub use services::Services;

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

#[cfg(feature = "tokio-std")]
impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex + ConstInit,
    P: Profile,
{
    pub fn new_arc(p: P) -> std::sync::Arc<Self> {
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
    ) -> Result<(), NetStackSendError> {
        self.inner
            .with_lock(|inner| inner.send_raw(hdr, hdr_raw, body))
    }

    /// Send a typed message
    pub fn send_ty<T: 'static + Serialize + Clone>(
        &self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_ty(hdr, t))
    }

    pub fn send_bor<T: Serialize>(&self, hdr: &Header, t: &T) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_bor(hdr, t))
    }

    pub fn send_err(&self, hdr: &Header, err: ProtocolError) -> Result<(), NetStackSendError> {
        self.inner.with_lock(|inner| inner.send_err(hdr, err))
    }

    pub(crate) unsafe fn try_attach_socket(&self, mut node: NonNull<SocketHeader>) -> Option<u8> {
        self.inner.with_lock(|inner| {
            let new_port = inner.alloc_port()?;
            unsafe {
                node.as_mut().port = new_port;
            }

            inner.sockets.push_front(node);
            Some(new_port)
        })
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

// Ergot Beep Boop AJMAJM
impl<R, P> NetStack<R, P>
where
    R: ScopedRawMutex,
    P: Profile,
{
    /// Perform an [`Endpoint`] Request, and await Response.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::profiles::null::Null;
    /// use ergot::socket::endpoint::std_bounded::Server;
    /// use ergot::Address;
    /// // Define an example endpoint
    /// ergot::endpoint!(Example, u32, i32, "pathho");
    ///
    /// static STACK: NetStack<CSRMutex, Null> = NetStack::new();
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     // (not shown: starting an `Example` service...)
    ///     # let jhdl = tokio::task::spawn(async {
    ///     #     println!("Serve!");
    ///     #     let srv = STACK.std_bounded_endpoint_server::<Example>(16, None);
    ///     #     let srv = core::pin::pin!(srv);
    ///     #     let mut hdl = srv.attach();
    ///     #     hdl.serve(async |p| *p as i32).await.unwrap();
    ///     #     println!("Served!");
    ///     # });
    ///     # // TODO: let the server attach first
    ///     # tokio::task::yield_now().await;
    ///     # tokio::time::sleep(core::time::Duration::from_millis(50)).await;
    ///     // Make a ping request to local
    ///     let res = STACK.req_resp::<Example>(
    ///         Address::unknown(),
    ///         &42u32,
    ///         None,
    ///     ).await;
    ///     assert_eq!(res, Ok(42i32));
    ///     # jhdl.await.unwrap();
    /// }
    /// ```
    pub async fn req_resp<E>(
        &self,
        dst: Address,
        req: &E::Request,
        name: Option<&str>,
    ) -> Result<E::Response, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        let resp = self.req_resp_full::<E>(dst, req, name).await?;
        Ok(resp.t)
    }

    /// Same as [`Self::req_resp`], but also returns the full message with header
    pub async fn req_resp_full<E>(
        &self,
        dst: Address,
        req: &E::Request,
        name: Option<&str>,
    ) -> Result<HeaderMessage<E::Response>, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        // Response doesn't need a name because we will reply back.
        //
        // We can also use a "single"/oneshot response because we know
        // this request will get exactly one response.
        let resp_sock = self.stack_single_endpoint_client::<E>();
        let resp_sock = pin!(resp_sock);
        let mut resp_hdl = resp_sock.attach();

        // If the destination is wildcard, include the any_all appendix to the
        // header
        let any_all = match dst.port_id {
            0 => Some(AnyAllAppendix {
                key: Key(E::REQ_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            255 => {
                return Err(ReqRespError::NoBroadcast);
            }
            _ => None,
        };

        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: resp_hdl.port(),
            },
            dst,
            any_all,
            seq_no: None,
            kind: FrameKind::ENDPOINT_REQ,
            ttl: DEFAULT_TTL,
        };
        self.send_ty(&hdr, req).map_err(ReqRespError::Local)?;
        // TODO: assert seq nos match somewhere? do we NEED seq nos if we have
        // port ids now?
        let resp = resp_hdl.recv().await;
        match resp {
            Ok(msg) => Ok(msg),
            Err(e) => Err(ReqRespError::Remote(e.t)),
        }
    }

    pub fn stack_single_endpoint_client<E: Endpoint>(
        &self,
    ) -> crate::socket::endpoint::single::Client<E, &'_ Self>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::single::Client::new(self, None)
    }

    pub fn stack_single_endpoint_server<E: Endpoint>(
        &self,
        name: Option<&str>,
    ) -> crate::socket::endpoint::single::Server<E, &'_ Self>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::single::Server::new(self, name)
    }

    pub fn stack_bounded_endpoint_server<E: Endpoint, const N: usize>(
        &self,
        name: Option<&str>,
    ) -> crate::socket::endpoint::stack_vec::Server<E, &'_ Self, N>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::stack_vec::Server::new(self, name)
    }

    #[cfg(feature = "tokio-std")]
    pub fn std_bounded_endpoint_server<E: Endpoint>(
        &self,
        bound: usize,
        name: Option<&str>,
    ) -> crate::socket::endpoint::std_bounded::Server<E, &'_ Self>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::std_bounded::Server::new(self, bound, name)
    }

    pub fn stack_single_topic_receiver<T>(
        &self,
        name: Option<&str>,
    ) -> crate::socket::topic::single::Receiver<T, &'_ Self>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::single::Receiver::new(self, name)
    }

    pub fn stack_bounded_topic_receiver<T, const N: usize>(
        &self,
        name: Option<&str>,
    ) -> crate::socket::topic::stack_vec::Receiver<T, &'_ Self, N>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::stack_vec::Receiver::new(self, name)
    }

    #[cfg(feature = "tokio-std")]
    pub fn std_bounded_topic_receiver<T>(
        &self,
        bound: usize,
        name: Option<&str>,
    ) -> crate::socket::topic::std_bounded::Receiver<T, &'_ Self>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::std_bounded::Receiver::new(self, bound, name)
    }

    #[cfg(feature = "tokio-std")]
    pub fn std_borrowed_topic_receiver<T>(
        &self,
        bound: usize,
        name: Option<&str>,
        mtu: u16,
    ) -> crate::socket::topic::stack_bor::Receiver<
        crate::interface_manager::utils::std::StdQueue,
        T,
        &Self,
    >
    where
        T: Topic,
        T::Message: Serialize + Sized,
    {
        let queue = crate::interface_manager::utils::std::new_std_queue(bound);
        crate::socket::topic::stack_bor::Receiver::new(self, queue, mtu, name)
    }

    /// Send a broadcast message for the topic `T`.
    ///
    /// This message will be sent to all matching local socket listeners, as well
    /// as on all interfaces, to be repeated outwards, in a "flood" style.
    pub fn broadcast_topic<T>(
        &self,
        msg: &T::Message,
        name: Option<&str>,
    ) -> Result<(), NetStackSendError>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
    {
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: 0,
            },
            dst: Address {
                network_id: 0,
                node_id: 0,
                port_id: 255,
            },
            any_all: Some(AnyAllAppendix {
                key: Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: DEFAULT_TTL,
        };
        self.send_ty(&hdr, msg)?;
        Ok(())
    }

    /// Send a broadcast message for the topic `T`.
    ///
    /// This message will be sent to all matching local socket listeners, as well
    /// as on all interfaces, to be repeated outwards, in a "flood" style.
    ///
    /// The same as [Self::broadcast_topic], but accepts messages with borrowed contents.
    /// This may be less efficient when delivering to local sockets.
    pub fn broadcast_topic_bor<T>(
        &self,
        msg: &T::Message,
        name: Option<&str>,
    ) -> Result<(), NetStackSendError>
    where
        T: Topic + Sized,
        T::Message: Serialize + Sized,
    {
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: 0,
            },
            dst: Address {
                network_id: 0,
                node_id: 0,
                port_id: 255,
            },
            any_all: Some(AnyAllAppendix {
                key: Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: DEFAULT_TTL,
        };
        self.send_bor(&hdr, msg)?;
        Ok(())
    }

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
        _ = self.broadcast_topic_bor::<ErgotFmtTxTopic>(&ErgotFmtTx { level, inner: args }, None);
    }

    pub fn services(&self) -> Services<&Self> {
        Services { inner: self }
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
