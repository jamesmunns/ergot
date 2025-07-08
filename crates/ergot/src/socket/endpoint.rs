//! Endpoint Client and Server Sockets
//!
//! TODO: Explanation of storage choices and examples using `single`.
use crate::{interface_manager::InterfaceManager, traits::Endpoint};
use core::pin::{Pin, pin};
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base::{self as base, socket::Response};

macro_rules! endpoint_server {
    ($sto: ty, $($arr: ident)?) => {
        /// An endpoint Server Socket, that accepts incoming `E::Request`s.
        #[pin_project::pin_project]
        pub struct Server<E, R, M, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            #[pin]
            sock: $crate::socket::endpoint::raw::Server<$sto, E, R, M>,
        }

        /// An endpoint Server handle
        pub struct ServerHandle<'a, E, R, M, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            hdl: super::raw::ServerHandle<'a, $sto, E, R, M>,
        }


        impl<E, R, M, $(const $arr: usize)?> Server<E, R, M, $($arr)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            /// Attach the Server to a Netstack and receive a Handle
            pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, E, R, M, $($arr)?> {
                let this = self.project();
                let hdl: super::raw::ServerHandle<'_, _, _, R, M> = this.sock.attach();
                ServerHandle { hdl }
            }
        }

        impl<E, R, M, $(const $arr: usize)?> ServerHandle<'_, E, R, M, $($arr)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            /// The port number of this server handle
            pub fn port(&self) -> u8 {
                self.hdl.port()
            }

            /// Manually receive an incoming packet, without automatically
            /// sending a response
            pub async fn recv_manual(&mut self) -> Response<E::Request> {
                self.hdl.recv_manual().await
            }

            /// Wait for an incoming packet, and respond using the given async closure
            pub async fn serve<F: AsyncFnOnce(&E::Request) -> E::Response>(
                &mut self,
                f: F,
            ) -> Result<(), base::net_stack::NetStackSendError>
            where
                E::Response: Serialize + Clone + DeserializeOwned + 'static,
            {
                self.hdl.serve(f).await
            }

            /// Wait for an incoming packet, and respond using the given blocking closure
            pub async fn serve_blocking<F: FnOnce(&E::Request) -> E::Response>(
                &mut self,
                f: F,
            ) -> Result<(), base::net_stack::NetStackSendError>
            where
                E::Response: Serialize + Clone + DeserializeOwned + 'static,
            {
                self.hdl.serve_blocking(f).await
            }
        }
    };
}

macro_rules! endpoint_client {
    ($sto: ty, $($arr: ident)?) => {
        /// An endpoint Client socket, typically used for receiving a response
        #[pin_project]
        pub struct Client<E, R, M, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            #[pin]
            sock: super::raw::Client<$sto, E, R, M>,
        }

        /// An endpoint Client Handle
        pub struct ClientHandle<'a, E, R, M, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            hdl: super::raw::ClientHandle<'a, $sto, E, R, M>,
        }

        impl<E, R, M, $(const $arr: usize)?> Client<E, R, M, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            /// Attach the Client socket to the net stack, and receive a Handle
            pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, E, R, M, $($arr)?> {
                let this = self.project();
                let hdl: super::raw::ClientHandle<'_, _, _, R, M> = this.sock.attach();
                ClientHandle { hdl }
            }
        }

        impl<E, R, M, $(const $arr: usize)?> ClientHandle<'_, E, R, M, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            R: ScopedRawMutex + 'static,
            M: InterfaceManager + 'static,
        {
            /// The port of this Client socket
            pub fn port(&self) -> u8 {
                self.hdl.port()
            }

            /// Receive a single response
            pub async fn recv(&mut self) -> Response<E::Response> {
                self.hdl.recv().await
            }
        }
    };
}

/// A raw Client/Server, generic over the [`Storage`](base::socket::raw_owned::Storage) impl.
pub mod raw {
    use super::*;
    use ergot_base::{
        FrameKind,
        socket::{
            Attributes,
            raw_owned::{self, Storage},
        },
    };

    #[pin_project]
    pub struct Server<S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: raw_owned::Socket<S, E::Request, R, M>,
    }

    #[pin_project]
    pub struct Client<S, E, R, M>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: raw_owned::Socket<S, E::Response, R, M>,
    }

    pub struct ServerHandle<'a, S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: raw_owned::SocketHdl<'a, S, E::Request, R, M>,
    }

    pub struct ClientHandle<'a, S, E, R, M>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: raw_owned::SocketHdl<'a, S, E::Response, R, M>,
    }

    impl<S, E, R, M> Server<S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, sto: S, name: Option<&str>) -> Self {
            Self {
                sock: raw_owned::Socket::new(
                    &net.inner,
                    base::Key(E::REQ_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                    sto,
                    name,
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, S, E, R, M> {
            let this = self.project();
            let hdl: raw_owned::SocketHdl<'_, S, E::Request, R, M> = this.sock.attach();
            ServerHandle { hdl }
        }
    }

    impl<S, E, R, M> ServerHandle<'_, S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

        pub async fn recv_manual(&mut self) -> Response<E::Request> {
            self.hdl.recv().await
        }

        pub async fn serve<F: AsyncFnOnce(&E::Request) -> E::Response>(
            &mut self,
            f: F,
        ) -> Result<(), base::net_stack::NetStackSendError>
        where
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
        {
            let msg = loop {
                let res = self.hdl.recv().await;
                match res {
                    Ok(req) => break req,
                    // TODO: Anything with errs? If not, change vtable
                    Err(_) => continue,
                }
            };
            let base::socket::HeaderMessage { hdr, t } = msg;
            let resp = f(&t).await;

            // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
            let hdr: base::Header = base::Header {
                src: hdr.dst,
                dst: hdr.src,
                // TODO: we never reply to an any/all, so don't include that info
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: base::FrameKind::ENDPOINT_RESP,
                ttl: base::DEFAULT_TTL,
            };
            self.hdl.stack().send_ty::<E::Response>(&hdr, &resp)
        }

        pub async fn serve_blocking<F: FnOnce(&E::Request) -> E::Response>(
            &mut self,
            f: F,
        ) -> Result<(), base::net_stack::NetStackSendError>
        where
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
        {
            let msg = loop {
                let res = self.hdl.recv().await;
                match res {
                    Ok(req) => break req,
                    // TODO: Anything with errs? If not, change vtable
                    Err(_) => continue,
                }
            };
            let base::socket::HeaderMessage { hdr, t } = msg;
            let resp = f(&t);

            // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
            let hdr: base::Header = base::Header {
                src: hdr.dst,
                dst: hdr.src,
                // TODO: we never reply to an any/all, so don't include that info
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: base::FrameKind::ENDPOINT_RESP,
                ttl: base::DEFAULT_TTL,
            };
            self.hdl.stack().send_ty::<E::Response>(&hdr, &resp)
        }
    }

    impl<S, E, R, M> Client<S, E, R, M>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, sto: S, name: Option<&str>) -> Self {
            Self {
                sock: raw_owned::Socket::new(
                    &net.inner,
                    base::Key(E::RESP_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_RESP,
                        discoverable: false,
                    },
                    sto,
                    name,
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, S, E, R, M> {
            let this = self.project();
            let hdl: raw_owned::SocketHdl<'_, S, E::Response, R, M> = this.sock.attach();
            ClientHandle { hdl }
        }
    }

    impl<S, E, R, M> ClientHandle<'_, S, E, R, M>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

        pub async fn recv(&mut self) -> Response<E::Response> {
            self.hdl.recv().await
        }
    }
}

/// Endpoint Client/Server sockets using [`Option<T>`] storage
pub mod single {
    use super::*;

    endpoint_server!(Option<Response<E::Request>>,);

    impl<E, R, M> Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, None, name),
            }
        }
    }

    endpoint_client!(Option<Response<E::Response>>,);

    impl<E, R, M> Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Client::new(net, None, name),
            }
        }
    }
}

/// Endpoint Client/Server sockets using [`stack_vec::Bounded`](base::socket::owned::stack_vec::Bounded) storage
pub mod stack_vec {
    use ergot_base::socket::owned::stack_vec::Bounded;

    use super::*;

    endpoint_server!(Bounded<Response<E::Request>, N>, N);

    impl<E, R, M, const N: usize> Server<E, R, M, N>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, Bounded::new(), name),
            }
        }
    }

    endpoint_client!(Bounded<Response<E::Response>, N>, N);

    impl<E, R, M, const N: usize> Client<E, R, M, N>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Client::new(net, Bounded::new(), name),
            }
        }
    }
}

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

/// Endpoint Client/Server sockets using [`std_bounded::Bounded`](base::socket::owned::std_bounded::Bounded) storage
#[cfg(feature = "std")]
pub mod std_bounded {
    use ergot_base::socket::owned::std_bounded::Bounded;

    use super::*;

    endpoint_server!(Bounded<Response<E::Request>>,);

    impl<E, R, M> Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, Bounded::with_bound(bound), name),
            }
        }
    }

    endpoint_client!(Bounded<Response<E::Response>>,);

    impl<E, R, M> Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Client::new(net, Bounded::with_bound(bound), name),
            }
        }
    }
}
