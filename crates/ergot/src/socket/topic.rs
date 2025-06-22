use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Topic;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base as base;

use super::{
    owned::{OwnedSocket, OwnedSocketHdl},
    std_bounded::{StdBoundedSocket, StdBoundedSocketHdl},
};

#[pin_project]
pub struct OwnedTopicSocket<T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    sock: OwnedSocket<T::Message, R, M>,
}

impl<T, R, M> OwnedTopicSocket<T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
        Self {
            sock: OwnedSocket::new_topic_in::<T>(net),
        }
    }

    pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> OwnedTopicSocketHdl<'a, T, R, M> {
        let this = self.project();
        let hdl: OwnedSocketHdl<'_, T::Message, R, M> = this.sock.attach_broadcast();
        OwnedTopicSocketHdl { hdl }
    }
}

pub struct OwnedTopicSocketHdl<'a, T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: OwnedSocketHdl<'a, T::Message, R, M>,
}

impl<T, R, M> OwnedTopicSocketHdl<'_, T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub async fn recv(&mut self) -> base::socket::OwnedMessage<T::Message> {
        self.hdl.recv().await
    }
}

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

#[pin_project]
pub struct StdBoundedTopicSocket<T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    sock: StdBoundedSocket<T::Message, R, M>,
}

impl<T, R, M> StdBoundedTopicSocket<T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new(stack: &'static base::net_stack::NetStack<R, M>, bound: usize) -> Self {
        Self {
            sock: StdBoundedSocket::new_topic_in::<T>(stack, bound),
        }
    }

    pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> StdBoundedTopicSocketHdl<'a, T, R, M> {
        let this = self.project();
        let hdl: StdBoundedSocketHdl<'_, T::Message, R, M> = this.sock.attach_broadcast();
        StdBoundedTopicSocketHdl { hdl }
    }
}

pub struct StdBoundedTopicSocketHdl<'a, T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: StdBoundedSocketHdl<'a, T::Message, R, M>,
}

impl<T, R, M> StdBoundedTopicSocketHdl<'_, T, R, M>
where
    T: Topic,
    T::Message: Serialize + Clone + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub async fn recv(&mut self) -> base::socket::OwnedMessage<T::Message> {
        self.hdl.recv().await
    }
}
