//! Topic Sockets
//!
//! TODO: Explanation of storage choices and examples using `single`.
use crate::traits::Topic;
use core::pin::{Pin, pin};
use pin_project::pin_project;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base as base;
use ergot_base::{
    FrameKind,
    socket::{Attributes, Response},
};

macro_rules! topic_receiver {
    ($sto: ty, $($arr: ident)?) => {
        /// A receiver of [`Topic`] messages.
        #[pin_project::pin_project]
        pub struct Receiver<T, NS, $(const $arr: usize)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            #[pin]
            sock: $crate::socket::topic::raw::Receiver<$sto, T, NS>,
        }

        /// A handle of an active [`Receiver`].
        ///
        /// Can be used to receive a stream of `T::Message` items.
        pub struct ReceiverHandle<'a, T, NS, $(const $arr: usize)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            hdl: $crate::socket::topic::raw::ReceiverHandle<'a, $sto, T, NS>,
        }

        impl<T, NS, $(const $arr: usize)?> Receiver<T, NS, $($arr)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            /// Attach to the [`NetStack`](crate::net_stack::NetStack), and obtain a [`ReceiverHandle`]
            pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, T, NS, $($arr)?> {
                let this = self.project();
                let hdl: $crate::socket::topic::raw::ReceiverHandle<'_, _, T, NS> = this.sock.subscribe();
                ReceiverHandle { hdl }
            }
        }

        impl<T, NS, $(const $arr: usize)?> ReceiverHandle<'_, T, NS, $($arr)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            /// Await the next successfully received `T::Message`
            pub async fn recv(&mut self) -> base::socket::HeaderMessage<T::Message> {
                self.hdl.recv().await
            }
        }

    };
}

/// A raw Receiver, generic over the [`Storage`](base::socket::raw_owned::Storage) impl.
pub mod raw {
    use super::*;

    /// A receiver of [`Topic`] messages.
    #[pin_project]
    pub struct Receiver<S, T, NS>
    where
        S: base::socket::raw_owned::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        #[pin]
        sock: base::socket::raw_owned::Socket<S, T::Message, NS>,
    }

    /// A handle of an active [`Receiver`].
    ///
    /// Can be used to receive a stream of `T::Message` items.
    pub struct ReceiverHandle<'a, S, T, NS>
    where
        S: base::socket::raw_owned::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        hdl: base::socket::raw_owned::SocketHdl<'a, S, T::Message, NS>,
    }

    impl<S, T, NS> Receiver<S, T, NS>
    where
        S: base::socket::raw_owned::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        /// Create a new Receiver with the given storage
        pub fn new(net: NS, sto: S, name: Option<&str>) -> Self {
            Self {
                sock: base::socket::raw_owned::Socket::new(
                    net.stack(),
                    base::Key(T::TOPIC_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::TOPIC_MSG,
                        discoverable: true,
                    },
                    sto,
                    name,
                ),
            }
        }

        /// Attach and obtain a ReceiverHandle
        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, S, T, NS> {
            let this = self.project();
            let hdl: base::socket::raw_owned::SocketHdl<'_, S, T::Message, NS> =
                this.sock.attach_broadcast();
            ReceiverHandle { hdl }
        }
    }

    impl<S, T, NS> ReceiverHandle<'_, S, T, NS>
    where
        S: base::socket::raw_owned::Storage<Response<T::Message>>,
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        /// Await the next successfully received `T::Message`
        pub async fn recv(&mut self) -> base::socket::HeaderMessage<T::Message> {
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

/// Topic sockets using [`Option<T>`] storage
pub mod single {
    use super::*;

    topic_receiver!(Option<Response<T::Message>>,);

    impl<T, NS> Receiver<T, NS>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        /// Create a new, empty, single slot receiver
        pub fn new(net: NS, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, None, name),
            }
        }
    }
}

// ---

/// Topic sockets using [`stack_vec::Bounded`](base::socket::owned::stack_vec::Bounded) storage
pub mod stack_vec {
    use ergot_base::socket::owned::stack_vec::Bounded;

    use super::*;

    topic_receiver!(Bounded<Response<T::Message>, N>, N);

    impl<T, NS, const N: usize> Receiver<T, NS, N>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        /// Create a new Receiver with room for `N` messages
        pub fn new(net: NS, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, Bounded::new(), name),
            }
        }
    }
}

/// Topic sockets using [`std_bounded::Bounded`](base::socket::owned::std_bounded::Bounded) storage
#[cfg(feature = "std")]
pub mod std_bounded {
    use ergot_base::socket::owned::std_bounded::Bounded;

    use super::*;

    topic_receiver!(Bounded<Response<T::Message>>,);

    impl<T, NS> Receiver<T, NS>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
        NS: base::net_stack::NetStackHandle,
    {
        /// Create a heap allocated Receiver with room for up to `N` messages
        pub fn new(net: NS, bound: usize, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Receiver::new(net, Bounded::with_bound(bound), name),
            }
        }
    }
}
