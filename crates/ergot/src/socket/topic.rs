//! Topic Sockets
//!
//! TODO: Explanation of storage choices and examples using `single`.
use crate::traits::Topic;
use core::pin::{Pin, pin};
use pin_project::pin_project;
use serde::{Serialize, de::DeserializeOwned};

use crate as base;
use crate::{
    FrameKind,
    socket::{Attributes, Response},
};

macro_rules! topic_receiver {
    ($sto: ty, $($arr: ident)?) => {
        pub type BoxedReceiverHandle<T, NS, $(const $arr: usize)?> = ReceiverHandle<'static, T, NS, $($arr)?>;

        /// A receiver of [`Topic`] messages.
        //
        // NOTE: Load bearing repr(transparent)!
        #[pin_project::pin_project]
        #[repr(transparent)]
        pub struct Receiver<T, NS, $(const $arr: usize)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
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
            NS: crate::net_stack::NetStackHandle,
        {
            hdl: $crate::socket::topic::raw::ReceiverHandle<'a, $sto, T, NS>,
        }

        impl<T, NS, $(const $arr: usize)?> Receiver<T, NS, $($arr)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            /// Attach to the [`NetStack`](crate::net_stack::NetStack), and obtain a [`ReceiverHandle`]
            pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, T, NS, $($arr)?> {
                let this = self.project();
                let hdl: $crate::socket::topic::raw::ReceiverHandle<'_, _, T, NS> = this.sock.subscribe();
                ReceiverHandle { hdl }
            }

            #[cfg(feature = "std")]
            pub fn subscribe_boxed(self: Pin<Box<Self>>) -> BoxedReceiverHandle<T, NS, $($arr)?> {
                // SAFETY: Receiver is repr(transparent) with the contained topic::raw::Receiver.
                let self_transparent: Pin<Box<$crate::socket::topic::raw::Receiver<$sto, T, NS>>> = unsafe {
                    core::mem::transmute(self)
                };
                let hdl: $crate::socket::topic::raw::ReceiverHandle<_, T, NS> = self_transparent.subscribe_boxed();
                ReceiverHandle { hdl }
            }

            /// Attach to the [`NetStack`](crate::net_stack::NetStack), and obtain a [`ReceiverHandle`]
            pub fn subscribe_unicast<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, T, NS, $($arr)?> {
                let this = self.project();
                let hdl: $crate::socket::topic::raw::ReceiverHandle<'_, _, T, NS> = this.sock.subscribe_unicast();
                ReceiverHandle { hdl }
            }
        }

        impl<T, NS, $(const $arr: usize)?> ReceiverHandle<'_, T, NS, $($arr)?>
        where
            T: Topic,
            T::Message: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            /// Return the port of this receiver
            ///
            /// If this was created with [`Receiver::subscribe()`], the port will always
            /// be `255`. If this was created with [`Receiver::subscribe_unicast()`], this
            /// will return a non-broadcast port
            pub fn port(&self) -> u8 {
                self.hdl.port()
            }

            /// Await the next successfully received `T::Message`
            pub async fn recv(&mut self) -> base::socket::HeaderMessage<T::Message> {
                self.hdl.recv().await
            }

            pub fn try_recv(&mut self) -> Option<base::socket::HeaderMessage<T::Message>> {
                self.hdl.try_recv()
            }
        }

    };
}

/// A raw Receiver, generic over the [`Storage`](base::socket::raw_owned::Storage) impl.
pub mod raw {
    use super::*;

    /// A receiver of [`Topic`] messages.
    //
    // NOTE: Load bearing repr(transparent)!
    #[pin_project]
    #[repr(transparent)]
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

        /// Attach and obtain a ReceiverHandle
        #[cfg(feature = "std")]
        pub fn subscribe_boxed(self: Pin<Box<Self>>) -> ReceiverHandle<'static, S, T, NS> {
            // SAFETY: Receiver is repr(transparent) with the contained raw_owned::SocketHdl.
            let self_transparent: Pin<Box<base::socket::raw_owned::Socket<S, T::Message, NS>>> =
                unsafe { core::mem::transmute(self) };
            let hdl: base::socket::raw_owned::SocketHdl<S, T::Message, NS> =
                self_transparent.attach_broadcast_boxed();
            ReceiverHandle { hdl }
        }

        /// Attach and obtain a ReceiverHandle
        pub fn subscribe_unicast<'a>(self: Pin<&'a mut Self>) -> ReceiverHandle<'a, S, T, NS> {
            let this = self.project();
            let hdl: base::socket::raw_owned::SocketHdl<'_, S, T::Message, NS> = this.sock.attach();
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
        /// Return the port of this receiver
        ///
        /// If this was created with [`Receiver::subscribe()`], the port will always
        /// be `255`. If this was created with [`Receiver::subscribe_unicast()`], this
        /// will return a non-broadcast port
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

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

        /// See if there is a message available
        pub fn try_recv(&mut self) -> Option<base::socket::HeaderMessage<T::Message>> {
            loop {
                // If there is NO message ready, then return None now
                let res = self.hdl.try_recv()?;

                // If there is a GOOD message ready, return Some now. Otherwise,
                // continue until we get a good message or there are NO messages ready
                if let Ok(msg) = res {
                    return Some(msg);
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
    use crate::socket::owned::stack_vec::Bounded;

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
    use crate::socket::owned::std_bounded::Bounded;

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

/// Topic sockets using borrowed sockets, able to receive `send_bor` messages
pub mod stack_bor {
    use core::pin::Pin;

    use crate::traits::Topic;
    use crate::{
        FrameKind, Key,
        exports::bbq2::traits::bbqhdl::BbqHandle,
        net_stack::NetStackHandle,
        socket::{
            Attributes,
            borrow::{ResponseGrant, Socket, SocketHdl},
        },
    };
    use serde::Serialize;

    #[pin_project::pin_project]
    pub struct Receiver<Q, T, NS>
    where
        Q: BbqHandle,
        T: Topic,
        T::Message: Serialize + Sized,
        NS: NetStackHandle,
    {
        #[pin]
        inner: Socket<Q, T::Message, NS>,
    }

    pub struct ReceiverHdl<'a, Q, T, NS>
    where
        Q: BbqHandle,
        T: Topic,
        T::Message: Serialize + Sized,
        NS: NetStackHandle,
    {
        inner: SocketHdl<'a, Q, T::Message, NS>,
    }

    impl<Q, T, NS> Receiver<Q, T, NS>
    where
        Q: BbqHandle,
        T: Topic,
        T::Message: Serialize + Sized,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, sto: Q, mtu: u16, name: Option<&str>) -> Self {
            Self {
                inner: Socket::new(
                    net.stack(),
                    Key(T::TOPIC_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::TOPIC_MSG,
                        discoverable: true,
                    },
                    sto,
                    mtu,
                    name,
                ),
            }
        }

        /// Attach to the [`NetStack`](crate::net_stack::NetStack), and obtain a [`ReceiverHdl`]
        pub fn subscribe<'a>(self: Pin<&'a mut Self>) -> ReceiverHdl<'a, Q, T, NS> {
            let this = self.project();
            let inner: SocketHdl<'_, Q, T::Message, NS> = this.inner.attach_broadcast();
            ReceiverHdl { inner }
        }
    }

    impl<Q, T, NS> ReceiverHdl<'_, Q, T, NS>
    where
        Q: BbqHandle,
        T: Topic,
        T::Message: Serialize + Sized,
        NS: NetStackHandle,
    {
        /// Await the next successfully received `T::Message`
        pub async fn recv(&mut self) -> ResponseGrant<Q, T::Message> {
            self.inner.recv().await
        }
    }
}
