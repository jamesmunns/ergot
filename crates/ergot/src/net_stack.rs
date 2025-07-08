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
use core::pin::pin;

use base::net_stack::NetStackSendError;
use mutex::{ConstInit, ScopedRawMutex};
use postcard_rpc::{Endpoint, Topic};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ergot_base::{Address, FrameKind, Header},
    interface_manager::{self, InterfaceManager},
};

use ergot_base::{self as base, ProtocolError};

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
pub struct NetStack<R: ScopedRawMutex, M: InterfaceManager> {
    pub(crate) inner: base::net_stack::NetStack<R, M>,
}

#[derive(Debug, PartialEq)]
pub enum ReqRespError {
    Local(NetStackSendError),
    Remote(ProtocolError),
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
            inner: base::net_stack::NetStack::new(),
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
            inner: base::net_stack::NetStack::const_new(r, m),
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
    /// [`BlockingMutex`]: mutex::BlockingMutex
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
        self.inner.with_interface_manager(f)
    }

    /// Perform an [`Endpoint`] Request, and await Response.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::null::NullInterfaceManager as NullIM;
    /// use ergot::socket::endpoint::std_bounded::Server;
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
    ///     #     let srv = Server::<Example, _, _>::new(&STACK, 16);
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
    ///     ).await;
    ///     assert_eq!(res, Ok(42i32));
    ///     # jhdl.await.unwrap();
    /// }
    /// ```
    pub async fn req_resp<E>(
        &'static self,
        dst: Address,
        req: &E::Request,
    ) -> Result<E::Response, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        let resp_sock = crate::socket::endpoint::single::Client::<E, R, M>::new(self);
        let resp_sock = pin!(resp_sock);
        let mut resp_hdl = resp_sock.attach();
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: resp_hdl.port(),
            },
            dst,
            key: Some(base::Key(E::REQ_KEY.to_bytes())),
            seq_no: None,
            kind: FrameKind::ENDPOINT_REQ,
            ttl: base::DEFAULT_TTL,
        };
        self.send_ty(&hdr, req).map_err(ReqRespError::Local)?;
        // TODO: assert seq nos match somewhere? do we NEED seq nos if we have
        // port ids now?
        let resp = resp_hdl.recv().await;
        match resp {
            Ok(msg) => Ok(msg.t),
            Err(e) => Err(ReqRespError::Remote(e.t)),
        }
    }

    pub async fn broadcast_topic<T>(
        &'static self,
        msg: &T::Message,
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
            key: Some(base::Key(T::TOPIC_KEY.to_bytes())),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: base::DEFAULT_TTL,
        };
        self.send_ty(&hdr, msg)?;
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
    /// [`Topic::TOPIC_KEY`]: postcard_rpc::Topic::TOPIC_KEY
    pub fn send_ty<T: 'static + Serialize + Clone>(
        &'static self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        self.inner.send_ty(hdr, t)
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
