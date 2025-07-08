//! The "Sockets"
//!
//! Ergot is oriented around type-safe sockets. Rather than TCP/IP sockets,
//! which provide users with either streams or frames of bytes (e.g. `[u8]`),
//! Ergot sockets are always of a certain Rust data type, such as structs or
//! enums. They provide an API very similar to "channels", a common way of
//! passing data around within Rust programs.
//!
//! When messages are received from outside of the current application/firmware,
//! messages are deserialized using the `postcard` serialization format, a
//! compact, non-self-describing, binary format.
//!
//! When messages are sent locally within a device, no serialization or
//! deserialization occurs, meaning that fundamentally sending data to an
//! Ergot socket locally has no cost over using a normal channel.
//!
//! In general: Sockets **receive**, and the NetStack **sends**.
//!
//! ### Non-stateful sockets
//!
//! Currently, sockets in Ergot are not stateful, meaning that they only serve
//! to receive messages. Replies may be made by sending a response to the
//! source address of the received message, using the [`NetStack`] to send
//! the response.
//!
//! Conceptually, this makes Ergot sockets similar to UDP sockets: delivery
//! is not guaranteed.
//!
//! ### A variety of sockets
//!
//! Ergot allows for different implementations of what a "socket" is, with
//! a common subset of functionality. Normally, this might sound like just the
//! problem to solve with a Rust `trait`, however as we would like to store
//! all of these items in a single intrusive linked list, this becomes
//! problematic.
//!
//! Instead, the pinned sockets all feature a common socket header, which
//! includes a hand-crafted vtable used to interact with the socket. This allows
//! us to have the moral equivalent to `List<dyn Socket>`, but in a way that
//! is easier to support on embedded devices without an allocator.
//!
//! This indirection allows us to be flexible both in intent of a socket, for
//! example a socket that expects a single one-shot response, or a socket that
//! expects a stream of requests; as well as flexible in the means of storage
//! of a socket, for example using stackful bounded queues of message on
//! embedded systems, or heapful unbounded queues of messages on systems with
//! an allocator.
//!
//! This approach of using a linked list, common header, and vtable, is NEARLY
//! IDENTICAL to how most async executors operate in Rust, particularly how
//! Tasks containing differently-typed Futures are handled by the executor
//! itself when it comes to polling or dropping a Task.
//!
//! [`NetStack`]: crate::NetStack

use core::{
    any::TypeId,
    ptr::{self, NonNull},
};

use crate::{FrameKind, HeaderSeq, Key, ProtocolError};
use cordyceps::{Linked, list::Links};

pub mod borrow;
pub mod owned;
pub mod raw_owned;

#[derive(Debug)]
pub struct Attributes {
    pub kind: FrameKind,
    // If true: participates in service discovery and responds to ANY delivery.
    // if false: is not included in service discovery, and only responds to specific port addressing.
    pub discoverable: bool,
}

#[derive(Debug)]
pub struct SocketHeader {
    pub(crate) links: Links<SocketHeader>,
    pub(crate) vtable: &'static SocketVTable,
    pub(crate) key: Key,
    pub(crate) attrs: Attributes,
    pub(crate) port: u8,
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

#[derive(Debug, Clone)]
pub struct SocketVTable {
    pub(crate) recv_owned: Option<RecvOwned>,
    pub(crate) recv_bor: Option<RecvBorrowed>,
    pub(crate) recv_raw: RecvRaw,
    pub(crate) recv_err: Option<RecvError>,
    // NOTE: We do *not* have a `drop` impl here, because the list
    // doesn't ACTUALLY own the nodes, so it is not responsible for dropping
    // them. They are naturally destroyed by their true owner.
}

#[derive(Debug)]
pub struct HeaderMessage<T> {
    pub hdr: HeaderSeq,
    pub t: T,
}

pub type Response<T> = Result<HeaderMessage<T>, HeaderMessage<ProtocolError>>;

// TODO: replace with header and handle kind and stuff right!

// Morally: &T, TypeOf<T>, src, dst
// If return OK: the type has been moved OUT of the source
// May serialize, or may be just moved.
pub type RecvOwned = fn(
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
pub type RecvBorrowed = fn(
    // The socket ptr
    NonNull<()>,
    // The T ptr
    NonNull<()>,
    // the header
    HeaderSeq,
) -> Result<(), SocketSendError>;
// Morally: it's a packet
// Never a serialize, sometimes a deserialize
pub type RecvRaw = fn(
    // The socket ptr
    NonNull<()>,
    // The packet
    &[u8],
    // the header
    HeaderSeq,
    // the raw header
    &[u8],
) -> Result<(), SocketSendError>;

pub type RecvError = fn(
    // The socket ptr
    NonNull<()>,
    // the header
    HeaderSeq,
    // The Error
    ProtocolError,
);

// --------------------------------------------------------------------------
// impl SocketHeader
// --------------------------------------------------------------------------

unsafe impl Linked<Links<SocketHeader>> for SocketHeader {
    type Handle = NonNull<SocketHeader>;

    fn into_ptr(r: Self::Handle) -> core::ptr::NonNull<Self> {
        r
    }

    unsafe fn from_ptr(ptr: core::ptr::NonNull<Self>) -> Self::Handle {
        ptr
    }

    unsafe fn links(target: NonNull<Self>) -> NonNull<Links<SocketHeader>> {
        // Safety: using `ptr::addr_of!` avoids creating a temporary
        // reference, which stacked borrows dislikes.
        let node = unsafe { ptr::addr_of_mut!((*target.as_ptr()).links) };
        unsafe { NonNull::new_unchecked(node) }
    }
}

impl SocketSendError {
    pub fn to_error(&self) -> ProtocolError {
        match self {
            SocketSendError::NoSpace => ProtocolError::SSE_NO_SPACE,
            SocketSendError::DeserFailed => ProtocolError::SSE_DESER_FAILED,
            SocketSendError::TypeMismatch => ProtocolError::SSE_TYPE_MISMATCH,
            SocketSendError::WhatTheHell => ProtocolError::SSE_WHAT_THE_HELL,
        }
    }
}
