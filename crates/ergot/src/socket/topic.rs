use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Topic;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base as base;

pub mod single {
    use ergot_base::{FrameKind, socket::Attributes};

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
        sock: base::socket::single::Socket<T::Message, R, M>,
    }

    pub struct OwnedTopicSocketHdl<'a, T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: base::socket::single::SocketHdl<'a, T::Message, R, M>,
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
                sock: base::socket::single::Socket::new(
                    &net.inner,
                    base::Key(T::TOPIC_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::TOPIC_MSG,
                        discoverable: true,
                    },
                ),
            }
        }

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> OwnedTopicSocketHdl<'a, T, R, M> {
            let this = self.project();
            let hdl: base::socket::single::SocketHdl<'_, T::Message, R, M> =
                this.sock.attach_broadcast();
            OwnedTopicSocketHdl { hdl }
        }
    }

    impl<T, R, M> OwnedTopicSocketHdl<'_, T, R, M>
    where
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

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

pub mod std_bounded {
    use ergot_base::{FrameKind, socket::Attributes};

    use super::*;

    #[pin_project]
    pub struct StdBoundedTopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: base::socket::std_bounded::Socket<T::Message, R, M>,
    }

    impl<T, R, M> StdBoundedTopicSocket<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(stack: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: base::socket::std_bounded::Socket::new(
                    &stack.inner,
                    base::Key(T::TOPIC_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::TOPIC_MSG,
                        discoverable: true,
                    },
                    bound,
                ),
            }
        }

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> TopicSocketHdl<'a, T, R, M> {
            let this = self.project();
            let hdl: base::socket::std_bounded::SocketHdl<'_, T::Message, R, M> =
                this.sock.attach_broadcast();
            TopicSocketHdl { hdl }
        }
    }

    pub struct TopicSocketHdl<'a, T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: base::socket::std_bounded::SocketHdl<'a, T::Message, R, M>,
    }

    impl<T, R, M> TopicSocketHdl<'_, T, R, M>
    where
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
