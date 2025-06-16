use core::any::TypeId;
use core::ptr::{self, NonNull};

use cordyceps::{Linked, list::Links};
use postcard_rpc::{Endpoint, Key, Topic};
use postcard_schema::Schema;
use postcard_schema::schema::NamedType;

use crate::HeaderSeq;

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

#[derive(Debug, Clone, Copy)]
pub enum SocketTy {
    EndpointReq(&'static EndpointData),
    EndpointResp(&'static EndpointData),
    TopicIn(&'static TopicData),
    // todo: TopicOut?
}

pub struct SocketHeader {
    pub(crate) links: Links<SocketHeader>,
    pub(crate) port: u8,
    pub(crate) kind: SocketTy,
    pub(crate) vtable: &'static SocketVTable,
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

impl SocketTy {
    pub const fn endpoint_req<E: Endpoint>() -> Self {
        Self::EndpointReq(&const { EndpointData::for_endpoint::<E>() })
    }

    pub const fn endpoint_resp<E: Endpoint>() -> Self {
        Self::EndpointResp(&const { EndpointData::for_endpoint::<E>() })
    }

    pub const fn topic_in<T: Topic>() -> Self {
        Self::TopicIn(&const { TopicData::for_topic::<T>() })
    }

    pub fn key(&self) -> Key {
        match *self {
            SocketTy::EndpointReq(endpoint_data) => endpoint_data.req_key,
            SocketTy::EndpointResp(endpoint_data) => endpoint_data.resp_key,
            SocketTy::TopicIn(topic_data) => topic_data.msg_key,
        }
    }
}

// --------------------------------------------------------------------------
// impl SocketHeader
// --------------------------------------------------------------------------

unsafe impl Linked<Links<SocketHeader>> for SocketHeader {
    type Handle = NonNull<SocketHeader>;

    fn into_ptr(r: Self::Handle) -> std::ptr::NonNull<Self> {
        r
    }

    unsafe fn from_ptr(ptr: std::ptr::NonNull<Self>) -> Self::Handle {
        ptr
    }

    unsafe fn links(target: NonNull<Self>) -> NonNull<Links<SocketHeader>> {
        // Safety: using `ptr::addr_of!` avoids creating a temporary
        // reference, which stacked borrows dislikes.
        let node = unsafe { ptr::addr_of_mut!((*target.as_ptr()).links) };
        unsafe { NonNull::new_unchecked(node) }
    }
}
