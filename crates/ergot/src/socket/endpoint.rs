//! Endpoint Client and Server Sockets
//!
//! TODO: Explanation of storage choices and examples using `single`.
use crate::traits::Endpoint;
use core::pin::Pin;
use pin_project::pin_project;
use serde::{Serialize, de::DeserializeOwned};

use crate::socket::HeaderMessage;
use crate::{
    self as base, Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, nash::NameHash,
    socket::Response,
};

macro_rules! endpoint_server {
    ($sto: ty, $($arr: ident)?) => {
        /// An endpoint Server Socket, that accepts incoming `E::Request`s.
        #[pin_project::pin_project]
        pub struct Server<E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            #[pin]
            sock: $crate::socket::endpoint::raw::Server<$sto, E, NS>,
        }

        /// An endpoint Server handle
        pub struct ServerHandle<'a, E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            hdl: super::raw::ServerHandle<'a, $sto, E, NS>,
        }


        impl<E, NS, $(const $arr: usize)?> Server<E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            /// Attach the Server to a Netstack and receive a Handle
            pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, E, NS, $($arr)?> {
                let this = self.project();
                let hdl: super::raw::ServerHandle<'_, _, _, NS> = this.sock.attach();
                ServerHandle { hdl }
            }
        }

        impl<E, NS, $(const $arr: usize)?> ServerHandle<'_, E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
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

            /// Wait for an incoming packet, and respond using the given async closure
            pub async fn serve_full<F: AsyncFnOnce(&HeaderMessage<E::Request>) -> E::Response>(
                &mut self,
                f: F,
            ) -> Result<(), base::net_stack::NetStackSendError>
            where
                E::Response: Serialize + Clone + DeserializeOwned + 'static,
            {
                self.hdl.serve_full(f).await
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
        #[repr(transparent)]
        #[pin_project]
        pub struct Client<E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            #[pin]
            sock: super::raw::Client<$sto, E, NS>,
        }

        /// An endpoint Client Handle
        pub struct ClientHandle<'a, E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            hdl: super::raw::ClientHandle<'a, $sto, E, NS>,
            request_pending: bool,
        }

        impl<E, NS, $(const $arr: usize)?> Client<E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            /// Attach the Client socket to the net stack, and receive a Handle
            pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, E, NS, $($arr)?> {
                let this = self.project();
                let hdl: super::raw::ClientHandle<'_, _, _, NS> = this.sock.attach();
                ClientHandle {
                    hdl,
                    request_pending: false,
                }
            }

            /// Attach a pinned boxed Client and return an owned, `'static`
            /// handle. This is useful when the response wait must be moved to
            /// another task after the request has already been sent.
            #[cfg(feature = "std")]
            pub fn attach_boxed(
                self: std::pin::Pin<Box<Self>>,
            ) -> ClientHandle<'static, E, NS, $($arr)?> {
                // SAFETY: `Client` only wraps the pinned raw Client, and the
                // projection is representation-transparent for this field.
                let raw: std::pin::Pin<Box<super::raw::Client<$sto, E, NS>>> =
                    unsafe { core::mem::transmute(self) };
                ClientHandle {
                    hdl: raw.attach_boxed(),
                    request_pending: false,
                }
            }
        }

        impl<E, NS, $(const $arr: usize)?> ClientHandle<'_, E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: crate::net_stack::NetStackHandle,
        {
            /// The port of this Client socket
            pub fn port(&self) -> u8 {
                self.hdl.port()
            }

            /// Send an endpoint request synchronously without waiting for its
            /// response.
            ///
            /// Once this returns `Ok`, the request has been committed to the
            /// netstack. Call [`Self::recv`] later to await the response. A
            /// Client socket identifies responses by its unique source port,
            /// so a second request is rejected until [`Self::recv`] receives
            /// the response to the outstanding request. A failed send does not
            /// mark a request as pending and may be retried.
            pub fn send_request(
                &mut self,
                dst: Address,
                req: &E::Request,
                name: Option<&str>,
            ) -> Result<(), base::net_stack::ReqRespError>
            where
                E::Request: Serialize + Clone + 'static,
            {
                if self.request_pending {
                    return Err(base::net_stack::ReqRespError::RequestPending);
                }

                let any_all = match dst.port_id {
                    0 => Some(AnyAllAppendix {
                        key: Key(E::REQ_KEY.to_bytes()),
                        nash: name.map(NameHash::new),
                    }),
                    255 => return Err(base::net_stack::ReqRespError::NoBroadcast),
                    _ => None,
                };
                let hdr = Header {
                    src: Address {
                        network_id: 0,
                        node_id: 0,
                        port_id: self.port(),
                    },
                    dst,
                    any_all,
                    seq_no: None,
                    kind: FrameKind::ENDPOINT_REQ,
                    ttl: DEFAULT_TTL,
                };
                self.hdl
                    .stack()
                    .send_ty(&hdr, req)
                    .map_err(base::net_stack::ReqRespError::Local)?;
                self.request_pending = true;
                Ok(())
            }

            /// Receive a single response
            pub async fn recv(&mut self) -> Response<E::Response> {
                let response = self.hdl.recv().await;
                self.request_pending = false;
                response
            }
        }
    };
}

/// A raw Client/Server, generic over the [`Storage`](base::socket::raw_owned::Storage) impl.
pub mod raw {
    use super::*;
    use crate::socket::HeaderMessage;
    use crate::{
        FrameKind,
        net_stack::NetStackHandle,
        socket::{
            Attributes,
            raw_owned::{self, Storage},
        },
    };

    #[pin_project]
    pub struct Server<S, E, NS>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        #[pin]
        sock: raw_owned::Socket<S, E::Request, NS>,
    }

    #[pin_project]
    #[repr(transparent)]
    pub struct Client<S, E, NS>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        #[pin]
        sock: raw_owned::Socket<S, E::Response, NS>,
    }

    pub struct ServerHandle<'a, S, E, NS>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        hdl: raw_owned::SocketHdl<'a, S, E::Request, NS>,
    }

    pub struct ClientHandle<'a, S, E, NS>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        hdl: raw_owned::SocketHdl<'a, S, E::Response, NS>,
    }

    impl<S, E, NS> Server<S, E, NS>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, sto: S, name: Option<&str>) -> Self {
            Self {
                sock: raw_owned::Socket::new(
                    net.stack(),
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

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, S, E, NS> {
            let this = self.project();
            let hdl: raw_owned::SocketHdl<'_, S, E::Request, NS> = this.sock.attach();
            ServerHandle { hdl }
        }
    }

    impl<S, E, NS> ServerHandle<'_, S, E, NS>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
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
                src: {
                    // modify the port to match our specific port, in case the dst was port 0
                    let mut src = hdr.dst;
                    src.port_id = self.port();
                    src
                },
                dst: hdr.src,
                // TODO: we never reply to an any/all, so don't include that info
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: base::FrameKind::ENDPOINT_RESP,
                ttl: base::DEFAULT_TTL,
            };
            self.hdl.stack().send_ty::<E::Response>(&hdr, &resp)
        }

        pub async fn serve_full<F: AsyncFnOnce(&HeaderMessage<E::Request>) -> E::Response>(
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
            let resp = f(&msg).await;
            let base::socket::HeaderMessage { hdr, .. } = msg;

            // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
            let hdr: base::Header = base::Header {
                src: {
                    // modify the port to match our specific port, in case the dst was port 0
                    let mut src = hdr.dst;
                    src.port_id = self.port();
                    src
                },
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
                src: {
                    // modify the port to match our specific port, in case the dst was port 0
                    let mut src = hdr.dst;
                    src.port_id = self.port();
                    src
                },
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

    impl<S, E, NS> Client<S, E, NS>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, sto: S, name: Option<&str>) -> Self {
            Self {
                sock: raw_owned::Socket::new(
                    net.stack(),
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

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, S, E, NS> {
            let this = self.project();
            let hdl: raw_owned::SocketHdl<'_, S, E::Response, NS> = this.sock.attach();
            ClientHandle { hdl }
        }

        #[cfg(feature = "std")]
        pub fn attach_boxed(self: Pin<Box<Self>>) -> ClientHandle<'static, S, E, NS> {
            // SAFETY: `Client` is representation-transparent over the raw
            // owned socket and pinning the wrapper pins that socket.
            let socket: Pin<Box<raw_owned::Socket<S, E::Response, NS>>> =
                unsafe { core::mem::transmute(self) };
            ClientHandle {
                hdl: socket.attach_boxed(),
            }
        }
    }

    impl<S, E, NS> ClientHandle<'_, S, E, NS>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

        pub fn stack(&self) -> NS::Target {
            self.hdl.stack()
        }

        pub async fn recv(&mut self) -> Response<E::Response> {
            self.hdl.recv().await
        }
    }
}

/// Endpoint Client/Server sockets using [`Option<T>`] storage
pub mod single {
    use crate::net_stack::NetStackHandle;

    use super::*;

    endpoint_server!(Option<Response<E::Request>>,);

    impl<E, NS> Server<E, NS>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, None, name),
            }
        }
    }

    endpoint_client!(Option<Response<E::Response>>,);

    impl<E, NS> Client<E, NS>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Client::new(net, None, name),
            }
        }
    }
}

/// Endpoint Client/Server sockets using [`stack_vec::Bounded`](base::socket::owned::stack_vec::Bounded) storage
pub mod stack_vec {
    use crate::{net_stack::NetStackHandle, socket::owned::stack_vec::Bounded};

    use super::*;

    endpoint_server!(Bounded<Response<E::Request>, N>, N);

    impl<E, NS, const N: usize> Server<E, NS, N>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, Bounded::new(), name),
            }
        }
    }

    endpoint_client!(Bounded<Response<E::Response>, N>, N);

    impl<E, NS, const N: usize> Client<E, NS, N>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, name: Option<&str>) -> Self {
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
    use crate::{net_stack::NetStackHandle, socket::owned::std_bounded::Bounded};

    use super::*;

    endpoint_server!(Bounded<Response<E::Request>>,);

    impl<E, NS> Server<E, NS>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, bound: usize, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Server::new(net, Bounded::with_bound(bound), name),
            }
        }
    }

    endpoint_client!(Bounded<Response<E::Response>>,);

    impl<E, NS> Client<E, NS>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, bound: usize, name: Option<&str>) -> Self {
            Self {
                sock: super::raw::Client::new(net, Bounded::with_bound(bound), name),
            }
        }
    }
}
