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

macro_rules! topic_receiver {
    ($sto: ty, $($arr: ident)?) => {
        #[pin_project::pin_project]
        pub struct Receiver<T, R, M, $(const $arr: usize)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            #[pin]
            sock: $crate::socket::topic::raw::Receiver<$sto, T, R, M>,
        }

        pub struct ReceiverHandle<'a, T, R, M, $(const $arr: usize)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            hdl: $crate::socket::topic::raw::ReceiverHandle<'a, $sto, T, R, M>,
        }

        impl<T, R, M, $(const $arr: usize)?> Receiver<T, R, M, $($arr)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, T, R, M, $($arr)?> {
                let this = self.project();
                let hdl: $crate::socket::topic::raw::ReceiverHandle<'_, _, T, R, M> = this.sock.subscribe();
                ReceiverHandle { hdl }
            }
        }

        impl<T, R, M, $(const $arr: usize)?> ReceiverHandle<'_, T, R, M, $($arr)?>
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

    };
}

pub mod raw {
    use super::*;

    #[pin_project]
    pub struct Receiver<S, T, R, M>
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

    pub struct ReceiverHandle<'a, S, T, R, M>
    where
        S: base::socket::raw::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: base::socket::raw::SocketHdl<'a, S, T::Message, R, M>,
    }

    impl<S, T, R, M> Receiver<S, T, R, M>
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

        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, S, T, R, M> {
            let this = self.project();
            let hdl: base::socket::raw::SocketHdl<'_, S, T::Message, R, M> =
                this.sock.attach_broadcast();
            ReceiverHandle { hdl }
        }
    }

    impl<S, T, R, M> ReceiverHandle<'_, S, T, R, M>
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

    topic_receiver!(Option<Response<T::Message>>,);

    impl<T, R, M> Receiver<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, None),
            }
        }
    }
}

// ---

pub mod stack_vec {
    use ergot_base::socket::stack_vec::Bounded;

    use super::*;

    topic_receiver!(Bounded<Response<T::Message>, N>, N);

    impl<T, R, M, const N: usize> Receiver<T, R, M, N>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, Bounded::new()),
            }
        }
    }
}

pub mod std_bounded {
    use ergot_base::socket::std_bounded::Bounded;

    use super::*;

    topic_receiver!(Bounded<Response<T::Message>>,);

    impl<T, R, M> Receiver<T, R, M>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, Bounded::with_bound(bound)),
            }
        }
    }
}
