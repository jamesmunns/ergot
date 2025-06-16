use core::{
    any::TypeId,
    ptr::{self, NonNull},
};

use crate::{FrameKind, HeaderSeq, Key};
use cordyceps::{Linked, list::Links};

pub mod owned;
pub mod std_bounded;

pub struct SocketHeader {
    pub(crate) links: Links<SocketHeader>,
    pub(crate) port: u8,
    pub(crate) kind: FrameKind,
    pub(crate) vtable: &'static SocketVTable,
    pub(crate) key: Key,
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
