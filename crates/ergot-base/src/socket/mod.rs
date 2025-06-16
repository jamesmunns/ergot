use core::any::TypeId;
use core::ptr::{self, NonNull};

use cordyceps::{Linked, list::Links};
use mutex::ScopedRawMutex;
use postcard_rpc::{Endpoint, Key, Topic};
use postcard_schema::Schema;
use postcard_schema::schema::NamedType;

use crate::interface_manager::InterfaceManager;
use crate::{HeaderSeq, NetStack};

pub mod endpoint;
pub mod owned;
pub mod std_bounded;

#[derive(Debug)]
pub struct EndpointData {
    pub path: &'static str,
    pub req_key: Key,
    pub resp_key: Key,
    pub req_schema: &'static NamedType,
    pub resp_schema: &'static NamedType,
}

#[derive(Debug)]
pub struct TopicData {
    pub path: &'static str,
    pub msg_key: Key,
    pub msg_schema: &'static NamedType,
}

pub struct SocketHeaderEndpointReq {
    pub(crate) links: Links<SocketHeaderEndpointReq>,
    pub(crate) port: u8,
    pub(crate) data: &'static EndpointData,
    pub(crate) vtable: &'static SocketVTable,
}

impl SocketHeaderEndpointReq {
    pub fn key(&self) -> Key {
        self.data.req_key
    }
}

pub struct SocketHeaderEndpointResp {
    pub(crate) links: Links<SocketHeaderEndpointResp>,
    pub(crate) port: u8,
    pub(crate) data: &'static EndpointData,
    pub(crate) vtable: &'static SocketVTable,
}

impl SocketHeaderEndpointResp {
    pub fn key(&self) -> Key {
        self.data.resp_key
    }
}

pub struct SocketHeaderTopicIn {
    pub(crate) links: Links<SocketHeaderTopicIn>,
    pub(crate) port: u8,
    pub(crate) data: &'static TopicData,
    pub(crate) vtable: &'static SocketVTable,
}

impl SocketHeaderTopicIn {
    pub fn key(&self) -> Key {
        self.data.msg_key
    }
}

// TODO: Way of signaling "socket consumed"?
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SocketSendError {
    NoSpace,
    DeserFailed,
    TypeMismatch,
    WhatTheHell,
}

#[derive(Clone)]
pub struct SocketVTable {
    pub(crate) send_owned: Option<SendOwned>,
    pub(crate) send_bor: Option<SendBorrowed>,
    pub(crate) send_raw: SendRaw,
    // NOTE: We do *not* have a `drop` impl here, because the list
    // doesn't ACTUALLY own the nodes, so it is not responsible for dropping
    // them. They are naturally destroyed by their true owner.
}

#[derive(Debug)]
pub struct OwnedMessage<T: 'static> {
    pub hdr: HeaderSeq,
    pub t: T,
}

// TODO: replace with header and handle kind and stuff right!

// Morally: &mut ManuallyDrop<T>, TypeOf<T>, src, dst
// If return OK: the type has been moved OUT of the source
// May serialize, or may be just moved.
pub type SendOwned = fn(
    // The socket ptr
    NonNull<()>,
    // The T ptr
    NonNull<()>,
    // the header
    HeaderSeq,
    // The T ty
    &TypeId,
) -> Result<(), SocketSendError>;
// Morally: &T, src, dst
// Always a serialize
pub type SendBorrowed = fn(
    // The socket ptr
    NonNull<()>,
    // The T ptr
    NonNull<()>,
    // the header
    HeaderSeq,
) -> Result<(), SocketSendError>;
// Morally: it's a packet
// Never a serialize, sometimes a deserialize
pub type SendRaw = fn(
    // The socket ptr
    NonNull<()>,
    // The packet
    &[u8],
    // the header
    HeaderSeq,
) -> Result<(), SocketSendError>;

impl EndpointData {
    pub const fn for_endpoint<E: Endpoint>() -> Self {
        Self {
            path: E::PATH,
            req_key: E::REQ_KEY,
            resp_key: E::RESP_KEY,
            req_schema: E::Request::SCHEMA,
            resp_schema: E::Response::SCHEMA,
        }
    }
}

impl TopicData {
    pub const fn for_topic<T: Topic>() -> Self {
        Self {
            path: T::PATH,
            msg_key: T::TOPIC_KEY,
            msg_schema: T::Message::SCHEMA,
        }
    }
}

// --------------------------------------------------------------------------
// impl SocketHeader
// --------------------------------------------------------------------------

unsafe impl Linked<Links<SocketHeaderEndpointReq>> for SocketHeaderEndpointReq {
    type Handle = NonNull<SocketHeaderEndpointReq>;

    fn into_ptr(r: Self::Handle) -> std::ptr::NonNull<Self> {
        r
    }

    unsafe fn from_ptr(ptr: std::ptr::NonNull<Self>) -> Self::Handle {
        ptr
    }

    unsafe fn links(target: NonNull<Self>) -> NonNull<Links<SocketHeaderEndpointReq>> {
        // Safety: using `ptr::addr_of!` avoids creating a temporary
        // reference, which stacked borrows dislikes.
        let node = unsafe { ptr::addr_of_mut!((*target.as_ptr()).links) };
        unsafe { NonNull::new_unchecked(node) }
    }
}

unsafe impl Linked<Links<SocketHeaderEndpointResp>> for SocketHeaderEndpointResp {
    type Handle = NonNull<SocketHeaderEndpointResp>;

    fn into_ptr(r: Self::Handle) -> std::ptr::NonNull<Self> {
        r
    }

    unsafe fn from_ptr(ptr: std::ptr::NonNull<Self>) -> Self::Handle {
        ptr
    }

    unsafe fn links(target: NonNull<Self>) -> NonNull<Links<SocketHeaderEndpointResp>> {
        // Safety: using `ptr::addr_of!` avoids creating a temporary
        // reference, which stacked borrows dislikes.
        let node = unsafe { ptr::addr_of_mut!((*target.as_ptr()).links) };
        unsafe { NonNull::new_unchecked(node) }
    }
}

unsafe impl Linked<Links<SocketHeaderTopicIn>> for SocketHeaderTopicIn {
    type Handle = NonNull<SocketHeaderTopicIn>;

    fn into_ptr(r: Self::Handle) -> std::ptr::NonNull<Self> {
        r
    }

    unsafe fn from_ptr(ptr: std::ptr::NonNull<Self>) -> Self::Handle {
        ptr
    }

    unsafe fn links(target: NonNull<Self>) -> NonNull<Links<SocketHeaderTopicIn>> {
        // Safety: using `ptr::addr_of!` avoids creating a temporary
        // reference, which stacked borrows dislikes.
        let node = unsafe { ptr::addr_of_mut!((*target.as_ptr()).links) };
        unsafe { NonNull::new_unchecked(node) }
    }
}

/// ## Safety
/// Some.
pub unsafe trait SocketHeader {
    /// ## Safety
    /// Some.
    unsafe fn attach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) -> Option<u8>;

    /// ## Safety
    /// Some.
    unsafe fn detach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    );

    fn port(&self) -> u8;
    fn key(&self) -> Key;
    fn vtable(&self) -> &'static SocketVTable;
}

unsafe impl SocketHeader for SocketHeaderTopicIn {
    #[inline(always)]
    unsafe fn attach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) -> Option<u8> {
        unsafe { stack.try_attach_socket_tpc_in(this) }
    }

    #[inline(always)]
    unsafe fn detach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) {
        unsafe {
            stack.detach_socket_tpc_in(this);
        }
    }

    fn port(&self) -> u8 {
        self.port
    }

    fn key(&self) -> Key {
        self.data.msg_key
    }

    fn vtable(&self) -> &'static SocketVTable {
        self.vtable
    }
}

unsafe impl SocketHeader for SocketHeaderEndpointReq {
    #[inline(always)]
    unsafe fn attach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) -> Option<u8> {
        unsafe { stack.try_attach_socket_edpt_req(this) }
    }

    #[inline(always)]
    unsafe fn detach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) {
        unsafe {
            stack.detach_socket_edpt_req(this);
        }
    }

    fn port(&self) -> u8 {
        self.port
    }

    fn key(&self) -> Key {
        self.data.req_key
    }

    fn vtable(&self) -> &'static SocketVTable {
        self.vtable
    }
}

unsafe impl SocketHeader for SocketHeaderEndpointResp {
    #[inline(always)]
    unsafe fn attach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) -> Option<u8> {
        unsafe { stack.try_attach_socket_edpt_resp(this) }
    }

    #[inline(always)]
    unsafe fn detach<R: ScopedRawMutex, M: InterfaceManager>(
        this: NonNull<Self>,
        stack: &'static NetStack<R, M>,
    ) {
        unsafe {
            stack.detach_socket_edpt_resp(this);
        }
    }

    fn port(&self) -> u8 {
        self.port
    }

    fn key(&self) -> Key {
        self.data.resp_key
    }

    fn vtable(&self) -> &'static SocketVTable {
        self.vtable
    }
}
