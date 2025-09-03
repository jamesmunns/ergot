//! "Owned" sockets
//!
//! "Owned" sockets require `T: 'static`, and store messages in their deserialized `T` form,
//! rather as serialized bytes.
//!
//! This module contains versions of the [`raw_owned`](crate::socket::raw_owned) socket types
//! that use a specific kind of storage.
//!
//! Currently we support:
//!
//! * `single` sockets, which use an `Option<T>` for storage, suitable for one-shot responses
//! * `stack_vec` sockets, which use a heapless `Deque` for storage, and have a const-generic
//!   size bound
//! * `std_bounded` socket, which use a `std` `VecDeque` with an upper limit on the capacity

macro_rules! wrapper {
    ($sto: ty, $($arr: ident)?) => {
        // NOTE: Load bearing repr(transparent)!
        #[repr(transparent)]
        pub struct Socket<T, NS, $(const $arr: usize)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            socket: $crate::socket::raw_owned::Socket<$sto, T, NS>,
        }

        pub struct SocketHdl<'a, T, NS, $(const $arr: usize)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            hdl: $crate::socket::raw_owned::SocketHdl<'a, $sto, T, NS>,
        }

        pub struct Recv<'a, 'b, T, NS, $(const $arr: usize)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            recv: $crate::socket::raw_owned::Recv<'a, 'b, $sto, T, NS>,
        }

        impl<T, NS, $(const $arr: usize)?> Socket<T, NS, $($arr)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            pub fn attach<'a>(self: core::pin::Pin<&'a mut Self>) -> SocketHdl<'a, T, NS, $($arr)?> {
                let socket: core::pin::Pin<&'a mut $crate::socket::raw_owned::Socket<$sto, T, NS>>
                    = unsafe { self.map_unchecked_mut(|me| &mut me.socket) };
                SocketHdl {
                    hdl: socket.attach(),
                }
            }

            #[cfg(feature = "std")]
            pub fn attach_boxed(self: std::pin::Pin<Box<Self>>) -> SocketHdl<'static, T, NS, $($arr)?> {
                // SAFETY: Socket is repr(transparent) with the contained raw_owned::Socket.
                let box_transparent: std::pin::Pin<Box<$crate::socket::raw_owned::Socket<$sto, T, NS>>> = unsafe { core::mem::transmute(self) };
                SocketHdl {
                    hdl: box_transparent.attach_boxed(),
                }
            }

            pub fn attach_broadcast<'a>(
                self: core::pin::Pin<&'a mut Self>,
            ) -> SocketHdl<'a, T, NS, $($arr)?> {
                let socket: core::pin::Pin<&'a mut $crate::socket::raw_owned::Socket<$sto, T, NS>>
                    = unsafe { self.map_unchecked_mut(|me| &mut me.socket) };
                SocketHdl {
                    hdl: socket.attach_broadcast(),
                }
            }

            pub fn stack(&self) -> NS::Target {
                self.socket.stack()
            }
        }

        impl<'a, T, NS, $(const $arr: usize)?> SocketHdl<'a, T, NS, $($arr)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            pub fn port(&self) -> u8 {
                self.hdl.port()
            }

            pub fn stack(&self) -> NS::Target {
                self.hdl.stack()
            }

            pub fn recv<'b>(&'b mut self) -> Recv<'b, 'a, T, NS, $($arr)?> {
                Recv {
                    recv: self.hdl.recv(),
                }
            }
        }

        impl<T, NS, $(const $arr: usize)?> Future for Recv<'_, '_, T, NS, $($arr)?>
        where
            T: Clone + serde::de::DeserializeOwned + 'static,
            NS: $crate::net_stack::NetStackHandle,
        {
            type Output = $crate::socket::Response<T>;

            fn poll(
                self: core::pin::Pin<&mut Self>,
                cx: &mut core::task::Context<'_>,
            ) -> core::task::Poll<Self::Output> {
                let recv: core::pin::Pin<&mut $crate::socket::raw_owned::Recv<'_, '_, $sto, T, NS>>
                    = unsafe { self.map_unchecked_mut(|me| &mut me.recv) };
                recv.poll(cx)
            }
        }
    };
}

pub mod single {
    use serde::de::DeserializeOwned;

    use crate::{
        Key,
        net_stack::NetStackHandle,
        socket::{Attributes, raw_owned},
    };

    impl<T: 'static> raw_owned::Storage<T> for Option<T> {
        #[inline]
        fn is_full(&self) -> bool {
            self.is_some()
        }

        #[inline]
        fn is_empty(&self) -> bool {
            self.is_none()
        }

        #[inline]
        fn push(&mut self, t: T) -> Result<(), raw_owned::StorageFull> {
            if self.is_some() {
                return Err(raw_owned::StorageFull);
            }
            *self = Some(t);
            Ok(())
        }

        #[inline]
        fn try_pop(&mut self) -> Option<T> {
            self.take()
        }
    }

    wrapper!(Option<crate::socket::Response<T>>,);

    impl<T, NS> Socket<T, NS>
    where
        T: Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        #[inline]
        pub const fn new(net: NS::Target, key: Key, attrs: Attributes, name: Option<&str>) -> Self {
            Self {
                socket: raw_owned::Socket::new(net, key, attrs, None, name),
            }
        }
    }
}

#[cfg(feature = "std")]
pub mod std_bounded {
    use serde::de::DeserializeOwned;
    use std::collections::VecDeque;

    use crate::{
        Key,
        net_stack::NetStackHandle,
        socket::{Attributes, raw_owned},
    };

    pub struct Bounded<T> {
        storage: std::collections::VecDeque<T>,
        max_len: usize,
    }

    impl<T> Bounded<T> {
        pub fn with_bound(bound: usize) -> Self {
            Self {
                storage: VecDeque::new(),
                max_len: bound,
            }
        }
    }

    impl<T: 'static> raw_owned::Storage<T> for Bounded<T> {
        #[inline]
        fn is_full(&self) -> bool {
            self.storage.len() >= self.max_len
        }

        #[inline]
        fn is_empty(&self) -> bool {
            self.storage.is_empty()
        }

        #[inline]
        fn push(&mut self, t: T) -> Result<(), raw_owned::StorageFull> {
            if self.is_full() {
                return Err(raw_owned::StorageFull);
            }
            self.storage.push_back(t);
            Ok(())
        }

        #[inline]
        fn try_pop(&mut self) -> Option<T> {
            self.storage.pop_front()
        }
    }

    wrapper!(Bounded<crate::socket::Response<T>>,);

    impl<T, NS> Socket<T, NS>
    where
        T: Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        #[inline]
        pub fn new(
            net: NS::Target,
            key: Key,
            attrs: Attributes,
            bound: usize,
            name: Option<&str>,
        ) -> Self {
            Self {
                socket: raw_owned::Socket::new(net, key, attrs, Bounded::with_bound(bound), name),
            }
        }
    }
}

pub mod stack_vec {
    use serde::de::DeserializeOwned;

    use crate::Key;
    use crate::net_stack::NetStackHandle;

    use crate::socket::{Attributes, raw_owned};

    pub struct Bounded<T: 'static, const N: usize> {
        storage: heapless::Deque<T, N>,
    }

    impl<T: 'static, const N: usize> Bounded<T, N> {
        pub const fn new() -> Self {
            Self {
                storage: heapless::Deque::new(),
            }
        }
    }

    impl<T: 'static, const N: usize> Default for Bounded<T, N> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: 'static, const N: usize> raw_owned::Storage<T> for Bounded<T, N> {
        #[inline]
        fn is_full(&self) -> bool {
            self.storage.is_full()
        }

        #[inline]
        fn is_empty(&self) -> bool {
            self.storage.is_empty()
        }

        #[inline]
        fn push(&mut self, t: T) -> Result<(), raw_owned::StorageFull> {
            self.storage
                .push_back(t)
                .map_err(|_| raw_owned::StorageFull)
        }

        #[inline]
        fn try_pop(&mut self) -> Option<T> {
            self.storage.pop_front()
        }
    }

    wrapper!(Bounded<crate::socket::Response<T>, N>, N);

    impl<T, NS, const N: usize> Socket<T, NS, N>
    where
        T: Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        #[inline]
        pub const fn new(net: NS::Target, key: Key, attrs: Attributes, name: Option<&str>) -> Self {
            Self {
                socket: raw_owned::Socket::new(net, key, attrs, Bounded::new(), name),
            }
        }
    }
}
