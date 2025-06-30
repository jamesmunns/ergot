use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Topic;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base as base;
use ergot_base::{
    FrameKind,
    socket::{Attributes, Response},
};
pub mod raw {

    use super::*;

    #[pin_project]
    pub struct TopicSocket<S, T, R, M>
    where
        S: base::socket::raw::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: base::socket::raw::Socket<S, T::Message, R, M>,
    }

    pub struct TopicSocketHdl<'a, S, T, R, M>
    where
        S: base::socket::raw::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: base::socket::raw::SocketHdl<'a, S, T::Message, R, M>,
    }

    impl<S, T, R, M> TopicSocket<S, T, R, M>
    where
        S: base::socket::raw::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, sto: S) -> Self {
            Self {
                sock: base::socket::raw::Socket::new(
                    &net.inner,
                    base::Key(T::TOPIC_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::TOPIC_MSG,
                        discoverable: true,
                    },
                    sto,
                ),
            }
        }

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> TopicSocketHdl<'a, S, T, R, M> {
            let this = self.project();
            let hdl: base::socket::raw::SocketHdl<'_, S, T::Message, R, M> =
                this.sock.attach_broadcast();
            TopicSocketHdl { hdl }
        }
    }

    impl<S, T, R, M> TopicSocketHdl<'_, S, T, R, M>
    where
        S: base::socket::raw::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub async fn recv(&mut self) -> base::socket::OwnedMessage<T::Message> {
            loop {
                let res = self.hdl.recv().await;
                // TODO: do anything with errors? If not - we can use a different vtable
                if let Ok(msg) = res {
                    return msg;
                }
            }
        }
    }
}

pub mod single {
    use super::*;

    #[pin_project]
    pub struct TopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::TopicSocket<Option<Response<T::Message>>, T, R, M>,
    }

    pub struct TopicSocketHdl<'a, T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::TopicSocketHdl<'a, Option<Response<T::Message>>, T, R, M>,
    }

    impl<T, R, M> TopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: super::raw::TopicSocket::new(net, None),
            }
        }

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> TopicSocketHdl<'a, T, R, M> {
            let this = self.project();
            let hdl: super::raw::TopicSocketHdl<'_, _, T, R, M> = this.sock.subscribe();
            TopicSocketHdl { hdl }
        }
    }

    impl<T, R, M> TopicSocketHdl<'_, T, R, M>
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
}

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

pub mod std_bounded {
    use ergot_base::socket::std_bounded::Bounded;

    use super::*;
    #[pin_project]
    pub struct TopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::TopicSocket<Bounded<Response<T::Message>>, T, R, M>,
    }

    pub struct TopicSocketHdl<'a, T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::TopicSocketHdl<'a, Bounded<Response<T::Message>>, T, R, M>,
    }

    impl<T, R, M> TopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: super::raw::TopicSocket::new(net, Bounded::with_bound(bound)),
            }
        }

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> TopicSocketHdl<'a, T, R, M> {
            let this = self.project();
            let hdl: super::raw::TopicSocketHdl<'_, _, T, R, M> = this.sock.subscribe();
            TopicSocketHdl { hdl }
        }
    }

    impl<T, R, M> TopicSocketHdl<'_, T, R, M>
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
}
