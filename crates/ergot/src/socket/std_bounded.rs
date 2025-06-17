use std::pin::Pin;

use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::{Endpoint, Topic};
use serde::{Serialize, de::DeserializeOwned};

use ergot_base::{self as base, FrameKind};

use crate::interface_manager::InterfaceManager;

// Owned Socket
#[pin_project]
pub struct StdBoundedSocket<T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    inner: base::socket::std_bounded::StdBoundedSocket<T, R, M>,
}

pub struct StdBoundedSocketHdl<'a, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    inner: base::socket::std_bounded::StdBoundedSocketHdl<'a, T, R, M>,
}

// ---- impls ----

// impl StdBoundedSocket

impl<T, R, M> StdBoundedSocket<T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new_topic_in<U: Topic>(
        net: &'static base::net_stack::NetStack<R, M>,
        bound: usize,
    ) -> Self {
        Self {
            inner: base::socket::std_bounded::StdBoundedSocket::new(
                net,
                base::Key(U::TOPIC_KEY.to_bytes()),
                FrameKind::TOPIC_MSG,
                bound,
            ),
        }
    }

    pub fn new_endpoint_req<E: Endpoint>(
        net: &'static base::net_stack::NetStack<R, M>,
        bound: usize,
    ) -> Self {
        Self {
            inner: base::socket::std_bounded::StdBoundedSocket::new(
                net,
                base::Key(E::REQ_KEY.to_bytes()),
                FrameKind::ENDPOINT_REQ,
                bound,
            ),
        }
    }

    pub fn new_endpoint_resp<E: Endpoint>(
        net: &'static base::net_stack::NetStack<R, M>,
        bound: usize,
    ) -> Self {
        Self {
            inner: base::socket::std_bounded::StdBoundedSocket::new(
                net,
                base::Key(E::RESP_KEY.to_bytes()),
                FrameKind::ENDPOINT_RESP,
                bound,
            ),
        }
    }

    pub fn stack(&self) -> &'static base::net_stack::NetStack<R, M> {
        self.inner.stack()
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> StdBoundedSocketHdl<'a, T, R, M> {
        let this = self.project();
        StdBoundedSocketHdl {
            inner: this.inner.attach(),
        }
        // TODO: once-check?
    }
}

// impl StdBoundedSocketHdl

impl<'a, T, R, M> StdBoundedSocketHdl<'a, T, R, M>
where
    T: Serialize + DeserializeOwned + 'static,
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
    // that since `NonNull<StdBoundedSocket<E>>` is !Sync, then this future can't be Send,
    // BUT impl'ing Sync unsafely on StdBoundedSocketHdl + StdBoundedSocket doesn't seem to help.
    pub fn recv<'b>(&'b mut self) -> base::socket::std_bounded::Recv<'b, 'a, T, R, M> {
        self.inner.recv()
    }
}
