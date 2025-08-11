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
//! [interface manager]: ergot_base::interface_manager
//!
//! In general, interacting with anything contained by the [`NetStack`] requires
//! locking of the [`BlockingMutex`] which protects the inner contents. This
//! is used both to allow sharing of the inner contents, but also to allow
//! `Drop` impls to remove themselves from the stack in a blocking manner.
//!
//! [`BlockingMutex`]: mutex::BlockingMutex
use core::{fmt::Arguments, pin::pin};

use base::net_stack::NetStackSendError;
use mutex::{ConstInit, ScopedRawMutex};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ergot_base::{Address, FrameKind, Header},
    fmtlog::{ErgotFmtTx, Level},
    interface_manager::{self, Profile},
    traits::{Endpoint, Topic},
    well_known::ErgotFmtTxTopic,
};

use ergot_base::{
    self as base, AnyAllAppendix, ProtocolError, nash::NameHash, net_stack::NetStackHandle,
    socket::HeaderMessage,
};

/// The `NetStack`
///
/// The `NetStack` is the primary interface for *sending* messages, as well as
/// adding new sockets and interfaces.
///
/// The `NetStack` contains two main items:
///
/// * A list of local sockets
/// * An Interface Manager, responsible for holding any interfaces the
///   `NetStack` may use.
///
/// ### One Main "Trick"
///
/// In general, whenever *multiple* items need to be stored in the `NetStack`,
/// they should be stored *intrusively*, or as elements in an intrusively linked
/// list. This allows devices without a heap allocator to effectively handle
/// a variable number of items.
///
/// Ergot heavily leverages a trick to allow ephemeral items (that may reside
/// on the stack) to be safely added to a static linked list: It requires that
/// items added to intrusive lists are [pinned], and that when the pinned items
/// are dropped, they MUST be removed from the list prior to dropping. This
/// guarantee is backed by a [`BlockingMutex`], which MUST be held whenever
/// interacting with the items of a linked list, including in the local context
/// where the items are defined, and especially including the `Drop` impl of
/// those items.
///
/// For single core microcontrollers, this has little impact: the mutex is held
/// whenever access to the stack occurs, and the mutex may not be held across
/// an await point. For larger system, this may lead to some non-ideal
/// contention across parallel threads, however it is intended that this mutex
/// is for as short of a time as possible.
///
/// [`BlockingMutex`]: mutex::BlockingMutex
/// [pinned]: https://doc.rust-lang.org/std/pin/
pub struct NetStack<R: ScopedRawMutex, M: Profile> {
    pub(crate) inner: base::net_stack::NetStack<R, M>,
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

// ---- impl NetStack ----

impl<'a, R, M> NetStackHandle for &'a NetStack<R, M>
where
    R: ScopedRawMutex,
    M: Profile,
{
    type Target = &'a base::net_stack::NetStack<R, M>;
    type Mutex = R;
    type Profile = M;

    fn stack(&self) -> Self::Target {
        &self.inner
    }
}

#[cfg(feature = "std")]
impl<R, M> NetStackHandle for ArcNetStack<R, M>
where
    R: ScopedRawMutex,
    M: Profile,
{
    type Mutex = R;
    type Profile = M;
    type Target = Self;

    fn stack(&self) -> Self::Target {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<R, M> NetStack<R, M>
where
    R: ScopedRawMutex + ConstInit,
    M: Profile + interface_manager::ConstInit,
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
            inner: base::net_stack::NetStack::new(),
        }
    }
}

impl<R, M> NetStack<R, M>
where
    R: ScopedRawMutex + ConstInit,
    M: Profile,
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
    pub const fn new_with_profile(m: M) -> Self {
        Self {
            inner: base::net_stack::NetStack::new_with_profile(m),
        }
    }
}

impl<R, M> NetStack<R, M>
where
    R: ScopedRawMutex,
    M: Profile,
{
    /// Manually create a new, uninitialized [`NetStack`].
    ///
    /// This method is useful if your [`ScopedRawMutex`] or [`Profile`]
    /// do not implement their corresponding `ConstInit` trait.
    ///
    /// In general, this is most often only needed for `loom` testing, and
    /// [`NetStack::new()`] should be used when possible.
    pub const fn const_new(r: R, m: M) -> Self {
        Self {
            inner: base::net_stack::NetStack::const_new(r, m),
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
    /// [`BlockingMutex`]: mutex::BlockingMutex
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
    pub fn manage_profile<F: FnOnce(&mut M) -> U, U>(&'static self, f: F) -> U {
        self.inner.manage_profile(f)
    }

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
        &'static self,
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
        &'static self,
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
                key: base::Key(E::REQ_KEY.to_bytes()),
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
            ttl: base::DEFAULT_TTL,
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

    /// Send a broadcast message for the topic `T`.
    ///
    /// This message will be sent to all matching local socket listeners, as well
    /// as on all interfaces, to be repeated outwards, in a "flood" style.
    pub fn broadcast_topic<T>(
        &'static self,
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
                key: base::Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: base::DEFAULT_TTL,
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
        &'static self,
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
                key: base::Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: base::DEFAULT_TTL,
        };
        self.send_bor(&hdr, msg)?;
        Ok(())
    }

    /// Send a raw (pre-serialized) message.
    ///
    /// This interface should almost never be used by end-users, and is instead
    /// typically used by interfaces to feed received messages into the
    /// [`NetStack`].
    pub fn send_raw(
        &'static self,
        hdr: &Header,
        hdr_raw: &[u8],
        body: &[u8],
    ) -> Result<(), NetStackSendError> {
        self.inner.send_raw(hdr, hdr_raw, body)
    }

    pub fn base(&'static self) -> &'static base::net_stack::NetStack<R, M> {
        &self.inner
    }

    /// Send a typed message
    ///
    /// This is less spicy than `send_raw`, but will likely be deprecated in
    /// favor of easier-to-hold-right methods like [`Self::req_resp()`]. The
    /// provided `Key` MUST match the type `T`, e.g. [`Endpoint::REQ_KEY`],
    /// [`Endpoint::RESP_KEY`], or [`Topic::TOPIC_KEY`].
    ///
    /// [`Topic::TOPIC_KEY`]: crate::traits::Topic::TOPIC_KEY
    pub fn send_ty<T: 'static + Serialize + Clone>(
        &'static self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner.send_ty(hdr, t)
    }

    /// Send a borrowed message
    ///
    /// Similar to [`Self::send_ty`], but doesn't require owned content. This
    /// may be less efficient for local socket destinations.
    pub fn send_bor<T: Serialize>(
        &'static self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner.send_bor(hdr, t)
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

    #[cfg(feature = "std")]
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

    #[cfg(feature = "std")]
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

    #[cfg(feature = "std")]
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

    /// Send a trace-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn trace_fmt(&'static self, args: &Arguments<'_>) {
        self.level_fmt(Level::Trace, args);
    }

    /// Send a debug-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn debug_fmt(&'static self, args: &Arguments<'_>) {
        self.level_fmt(Level::Debug, args);
    }

    /// Send an info-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn info_fmt(&'static self, args: &Arguments<'_>) {
        self.level_fmt(Level::Info, args);
    }

    /// Send a warn-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn warn_fmt(&'static self, args: &Arguments<'_>) {
        self.level_fmt(Level::Warn, args);
    }

    /// Send an error-level formatted message to the [`ErgotFmtTxTopic`] as a broadcast topic message
    ///
    /// You can use [`crate::fmt!`] or [`core::format_args!`] to create the value passed to this function
    #[inline(always)]
    pub fn error_fmt(&'static self, args: &Arguments<'_>) {
        self.level_fmt(Level::Error, args);
    }

    fn level_fmt(&'static self, level: Level, args: &Arguments<'_>) {
        _ = self.broadcast_topic_bor::<ErgotFmtTxTopic>(&ErgotFmtTx { level, inner: args }, None);
    }
}

impl<R, M> Default for NetStack<R, M>
where
    R: ScopedRawMutex + ConstInit,
    M: Profile + interface_manager::ConstInit,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
pub use arc_netstack::ArcNetStack;

#[cfg(feature = "std")]
mod arc_netstack {
    use std::{ops::Deref, sync::Arc};

    use super::*;

    /// An ArcNetStack is useful on std devices where it is easier to store the network stack as
    /// a heap allocated and reference counted item.
    pub struct ArcNetStack<R: ScopedRawMutex, M: Profile> {
        pub(crate) inner: Arc<base::net_stack::NetStack<R, M>>,
    }

    impl<R, M> ArcNetStack<R, M>
    where
        R: ScopedRawMutex,
        M: Profile,
    {
        /// Access the contained [`Profile`].
        ///
        /// Access to the [`Profile`] is made via the provided closure.
        /// The [`BlockingMutex`] is locked for the duration of this access,
        /// inhibiting all other usage of this [`NetStack`].
        ///
        /// This can be used to add new interfaces, obtain metadata, or other
        /// actions supported by the chosen [`Profile`].
        ///
        /// [`BlockingMutex`]: mutex::BlockingMutex
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
        pub fn manage_profile<F: FnOnce(&mut M) -> U, U>(&self, f: F) -> U {
            self.inner.manage_profile(f)
        }

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
                    key: base::Key(E::REQ_KEY.to_bytes()),
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
                ttl: base::DEFAULT_TTL,
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
                    key: base::Key(T::TOPIC_KEY.to_bytes()),
                    nash: name.map(NameHash::new),
                }),
                seq_no: None,
                kind: FrameKind::TOPIC_MSG,
                ttl: base::DEFAULT_TTL,
            };
            self.send_ty(&hdr, msg)?;
            Ok(())
        }

        pub fn broadcast_topic_bor<T>(
            &self,
            msg: &T::Message,
            name: Option<&str>,
        ) -> Result<(), NetStackSendError>
        where
            T: Topic,
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
                    key: base::Key(T::TOPIC_KEY.to_bytes()),
                    nash: name.map(NameHash::new),
                }),
                seq_no: None,
                kind: FrameKind::TOPIC_MSG,
                ttl: base::DEFAULT_TTL,
            };
            self.send_bor(&hdr, msg)?;
            Ok(())
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
            self.inner.send_raw(hdr, hdr_raw, body)
        }

        pub fn base(&self) -> Arc<base::net_stack::NetStack<R, M>> {
            self.inner.clone()
        }

        /// Send a typed message
        ///
        /// This is less spicy than `send_raw`, but will likely be deprecated in
        /// favor of easier-to-hold-right methods like [`Self::req_resp()`]. The
        /// provided `Key` MUST match the type `T`, e.g. [`Endpoint::REQ_KEY`],
        /// [`Endpoint::RESP_KEY`], or [`Topic::TOPIC_KEY`].
        ///
        /// [`Topic::TOPIC_KEY`]: crate::traits::Topic::TOPIC_KEY
        pub fn send_ty<T: 'static + Serialize + Clone>(
            &self,
            hdr: &Header,
            t: &T,
        ) -> Result<(), NetStackSendError> {
            self.inner.send_ty(hdr, t)
        }

        /// Send a borrowed message
        ///
        /// Similar to [`Self::send_ty`], but doesn't require owned content. This
        /// may be less efficient for local socket destinations.
        pub fn send_bor<T: Serialize>(&self, hdr: &Header, t: &T) -> Result<(), NetStackSendError> {
            self.inner.send_bor(hdr, t)
        }

        pub fn stack_single_endpoint_client<E: Endpoint>(
            &self,
        ) -> crate::socket::endpoint::single::Client<E, Self>
        where
            E::Request: Serialize + DeserializeOwned + Clone,
            E::Response: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::endpoint::single::Client::new(self.clone(), None)
        }

        pub fn stack_single_endpoint_server<E: Endpoint>(
            &self,
            name: Option<&str>,
        ) -> crate::socket::endpoint::single::Server<E, Self>
        where
            E::Request: Serialize + DeserializeOwned + Clone,
            E::Response: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::endpoint::single::Server::new(self.clone(), name)
        }

        pub fn stack_bounded_endpoint_server<E: Endpoint, const N: usize>(
            &self,
            name: Option<&str>,
        ) -> crate::socket::endpoint::stack_vec::Server<E, Self, N>
        where
            E::Request: Serialize + DeserializeOwned + Clone,
            E::Response: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::endpoint::stack_vec::Server::new(self.clone(), name)
        }

        pub fn std_bounded_endpoint_server<E: Endpoint>(
            &self,
            bound: usize,
            name: Option<&str>,
        ) -> crate::socket::endpoint::std_bounded::Server<E, Self>
        where
            E::Request: Serialize + DeserializeOwned + Clone,
            E::Response: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::endpoint::std_bounded::Server::new(self.clone(), bound, name)
        }

        pub fn stack_single_topic_receiver<T>(
            &self,
            name: Option<&str>,
        ) -> crate::socket::topic::single::Receiver<T, Self>
        where
            T: Topic,
            T::Message: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::topic::single::Receiver::new(self.clone(), name)
        }

        pub fn stack_bounded_topic_receiver<T, const N: usize>(
            &self,
            name: Option<&str>,
        ) -> crate::socket::topic::stack_vec::Receiver<T, Self, N>
        where
            T: Topic,
            T::Message: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::topic::stack_vec::Receiver::new(self.clone(), name)
        }

        pub fn std_bounded_topic_receiver<T>(
            &self,
            bound: usize,
            name: Option<&str>,
        ) -> crate::socket::topic::std_bounded::Receiver<T, Self>
        where
            T: Topic,
            T::Message: Serialize + DeserializeOwned + Clone,
        {
            crate::socket::topic::std_bounded::Receiver::new(self.clone(), bound, name)
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
            _ = self
                .broadcast_topic_bor::<ErgotFmtTxTopic>(&ErgotFmtTx { level, inner: args }, None);
        }
    }

    impl<R: ScopedRawMutex, M: Profile> Deref for ArcNetStack<R, M> {
        type Target = base::net_stack::NetStack<R, M>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<R: ScopedRawMutex + ConstInit, M: Profile> ArcNetStack<R, M> {
        pub fn new_with_profile(p: M) -> Self {
            Self {
                inner: base::net_stack::NetStack::new_arc(p),
            }
        }
    }

    impl<R: ScopedRawMutex + ConstInit, M: Profile + Default> ArcNetStack<R, M> {
        pub fn new() -> Self {
            Self {
                inner: base::net_stack::NetStack::new_arc(Default::default()),
            }
        }
    }

    impl<R: ScopedRawMutex, M: Profile> Clone for ArcNetStack<R, M> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
}
