use core::pin::Pin;

use base::interface_manager::InterfaceManager;
use ergot_base::{self as base, FrameKind, socket::Attributes};
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::{Endpoint, Topic};
use serde::{Serialize, de::DeserializeOwned};

// Owned Socket
#[repr(C)]
#[pin_project]
pub struct OwnedSocket<T, R, M>
where
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    inner: base::socket::owned::OwnedSocket<T, R, M>,
}

pub struct OwnedSocketHdl<'a, T, R, M>
where
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    inner: base::socket::owned::OwnedSocketHdl<'a, T, R, M>,
}

// ---- impls ----

// impl OwnedSocket

impl<T, R, M> OwnedSocket<T, R, M>
where
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new_topic_in<U: Topic<Message = T>>(net: &'static crate::NetStack<R, M>) -> Self {
        Self {
            inner: base::socket::owned::OwnedSocket::new(
                &net.inner,
                base::Key(U::TOPIC_KEY.to_bytes()),
                Attributes {
                    kind: FrameKind::TOPIC_MSG,
                    discoverable: true,
                },
            ),
        }
    }

    pub const fn new_endpoint_req<E: Endpoint<Request = T>>(
        net: &'static crate::NetStack<R, M>,
    ) -> Self {
        Self {
            inner: base::socket::owned::OwnedSocket::new(
                &net.inner,
                base::Key(E::REQ_KEY.to_bytes()),
                Attributes {
                    kind: FrameKind::ENDPOINT_REQ,
                    discoverable: true,
                },
            ),
        }
    }

    pub const fn new_endpoint_resp<E: Endpoint<Response = T>>(
        net: &'static crate::NetStack<R, M>,
    ) -> Self {
        Self {
            inner: base::socket::owned::OwnedSocket::new(
                &net.inner,
                base::Key(E::RESP_KEY.to_bytes()),
                Attributes {
                    kind: FrameKind::ENDPOINT_RESP,
                    discoverable: false,
                },
            ),
        }
    }
}

impl<T, R, M> OwnedSocket<T, R, M>
where
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn attach<'a>(self: Pin<&'a mut Self>) -> OwnedSocketHdl<'a, T, R, M> {
        let this = self.project();
        OwnedSocketHdl {
            inner: this.inner.attach(),
        }
    }

    pub fn attach_broadcast<'a>(self: Pin<&'a mut Self>) -> OwnedSocketHdl<'a, T, R, M> {
        let this = self.project();
        OwnedSocketHdl {
            inner: this.inner.attach_broadcast(),
        }
    }
}

// impl OwnedSocketHdl

impl<'a, T, R, M> OwnedSocketHdl<'a, T, R, M>
where
    T: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn port(&self) -> u8 {
        self.inner.port()
    }

    pub fn stack(&self) -> &'static base::net_stack::NetStack<R, M> {
        self.inner.stack()
    }

    // TODO: This future is !Send? I don't fully understand why, but rustc complains
    // that since `NonNull<OwnedSocket<E>>` is !Sync, then this future can't be Send,
    // BUT impl'ing Sync unsafely on OwnedSocketHdl + OwnedSocket doesn't seem to help.
    pub fn recv<'b>(&'b mut self) -> base::socket::owned::Recv<'b, 'a, T, R, M> {
        self.inner.recv()
    }
}
