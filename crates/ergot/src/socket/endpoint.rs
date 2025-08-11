//! Endpoint Client and Server Sockets
//!
//! TODO: Explanation of storage choices and examples using `single`.
use crate::traits::Endpoint;
use core::pin::{Pin, pin};
use pin_project::pin_project;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base::{self as base, socket::Response};

macro_rules! endpoint_server {
    ($sto: ty, $($arr: ident)?) => {
        /// An endpoint Server Socket, that accepts incoming `E::Request`s.
        #[pin_project::pin_project]
        pub struct Server<E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            #[pin]
            sock: $crate::socket::endpoint::raw::Server<$sto, E, NS>,
        }

        /// An endpoint Server handle
        pub struct ServerHandle<'a, E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            hdl: super::raw::ServerHandle<'a, $sto, E, NS>,
        }


        impl<E, NS, $(const $arr: usize)?> Server<E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Request: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
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
            NS: ergot_base::net_stack::NetStackHandle,
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
        pub struct Client<E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            #[pin]
            sock: super::raw::Client<$sto, E, NS>,
        }

        /// An endpoint Client Handle
        pub struct ClientHandle<'a, E, NS, $(const $arr: usize)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            hdl: super::raw::ClientHandle<'a, $sto, E, NS>,
        }

        impl<E, NS, $(const $arr: usize)?> Client<E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
        {
            /// Attach the Client socket to the net stack, and receive a Handle
            pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, E, NS, $($arr)?> {
                let this = self.project();
                let hdl: super::raw::ClientHandle<'_, _, _, NS> = this.sock.attach();
                ClientHandle { hdl }
            }
        }

        impl<E, NS, $(const $arr: usize)?> ClientHandle<'_, E, NS, $($arr)?>
        where
            E: Endpoint,
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
            NS: ergot_base::net_stack::NetStackHandle,
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

        pub async fn recv(&mut self) -> Response<E::Response> {
            self.hdl.recv().await
        }
    }
}

/// Endpoint Client/Server sockets using [`Option<T>`] storage
pub mod single {
    use ergot_base::net_stack::NetStackHandle;

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
    use ergot_base::{net_stack::NetStackHandle, socket::owned::stack_vec::Bounded};

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

pub mod req_bor_resp_owned {
    use core::{marker::PhantomData, ops::Deref, pin::Pin};

    use ergot_base::{
        exports::bbq2::traits::bbqhdl::BbqHandle, net_stack::NetStackHandle, socket::{
            borrow::{BorResponse, MessageGrant, Socket, SocketHdl}, Attributes, HeaderMessage
        }, FrameKind, Key, NetStackSendError
    };
    use serde::{Deserialize, Serialize};

    use crate::traits::Endpoint;

    #[pin_project::pin_project]
    pub struct Server<Q, E, NS>
    where
        Q: BbqHandle,
        E: Endpoint,
        E::Request: Serialize + Sized,
        NS: NetStackHandle,
    {
        #[pin]
        inner: Socket<Q, E::Request, NS>,
    }

    pub struct ServerHdl<'a, Q, E, NS>
    where
        Q: BbqHandle,
        E: Endpoint,
        E::Request: Serialize + Sized,
        NS: NetStackHandle,
    {
        inner: SocketHdl<'a, Q, E::Request, NS>,
    }

    impl<Q, E, NS> Server<Q, E, NS>
    where
        Q: BbqHandle,
        E: Endpoint,
        E::Request: Serialize + Sized,
        NS: NetStackHandle,
    {
        pub fn new(net: NS, sto: Q, mtu: u16, name: Option<&str>) -> Self {
            Self {
                inner: Socket::new(
                    net.stack(),
                    Key(E::REQ_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                    sto,
                    mtu,
                    name,
                ),
            }
        }

        /// Attach to the [`NetStack`](crate::net_stack::NetStack), and obtain a [`ReceiverHdl`]
        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHdl<'a, Q, E, NS> {
            let this = self.project();
            let inner: SocketHdl<'_, Q, E::Request, NS> = this.inner.attach();
            ServerHdl { inner }
        }
    }

    pub struct RequestGrant<Q: BbqHandle, E: Endpoint, NS: NetStackHandle> {
        inner: MessageGrant<Q, E::Request>,
        ns: NS::Target,
    }

    pub struct RequestGrantDecoded<'a, E: Endpoint, NS: NetStackHandle> {
        req: HeaderMessage<E::Request>,
        _pd: PhantomData<&'a mut [u8]>,
        nsh: NS::Target,
    }

    impl<'a, E: Endpoint, NS: NetStackHandle> RequestGrantDecoded<'a, E, NS> {
        pub fn reply(self, resp: &E::Response) -> Result<(), NetStackSendError>
        where
            E::Response: Serialize + Clone + 'static,
        {
            let hdr = self.req.hdr.clone();
            let nsh = self.nsh;

            // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
            let hdr: ergot_base::Header = ergot_base::Header {
                src: hdr.dst,
                dst: hdr.src,
                // TODO: we never reply to an any/all, so don't include that info
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: ergot_base::FrameKind::ENDPOINT_RESP,
                ttl: ergot_base::DEFAULT_TTL,
            };

            nsh.send_ty::<E::Response>(&hdr, resp)
        }
    }

    impl<'a, E: Endpoint, NS: NetStackHandle> Deref for RequestGrantDecoded<'a, E, NS> {
        type Target = E::Request;

        fn deref(&self) -> &Self::Target {
            &self.req.t
        }
    }

    impl<Q, E, NS> RequestGrant<Q, E, NS>
    where
        Q: BbqHandle,
        E: Endpoint,
        NS: NetStackHandle,
    {
        pub fn decode<'a>(&'a mut self) -> Option<RequestGrantDecoded<'a, E, NS>>
        where
            E::Request: Deserialize<'a>,
        {
            let r: BorResponse<'a, _> = self.inner.try_access()?;
            Some(RequestGrantDecoded {
                req: r.inner.ok()?,
                _pd: PhantomData,
                nsh: self.ns.clone(),
            })
        }
        // pub fn process_blocking<'a, F: FnOnce(&E::Request) -> E::Response>(
        //     self,
        //     f: F,
        // ) -> Result<(), ()>
        // where
        //     Q: 'a,
        //     E::Request: Deserialize<'a> + 'a,
        // {
        //     let hdr = self.inner.hdr.clone();
        //     let Some(a) = self.inner.try_access() else {
        //         return Err(());
        //     };
        //     let Ok(b) = a else {
        //         return Err(());
        //     };
        //     f(&b.t);

        //     todo!()
        // }
    }

    impl<'a, Q, E, NS> ServerHdl<'a, Q, E, NS>
    where
        Q: BbqHandle,
        E: Endpoint,
        E::Request: Serialize + Sized,
        NS: NetStackHandle,
    {
        /// The port number of this server handle
        pub fn port(&self) -> u8 {
            self.inner.port()
        }

        /// Manually receive an incoming packet, without automatically
        /// sending a response
        pub async fn recv_manual(&mut self) -> RequestGrant<Q, E, NS> {
            RequestGrant {
                inner: self.inner.recv().await,
                ns: self.inner.stack(),
            }
        }

        // /// Wait for an incoming packet, and respond using the given async closure
        // pub async fn serve<F: AsyncFnOnce(&E::Request) -> E::Response>(
        //     &mut self,
        //     f: F,
        // ) -> Result<(), ergot_base::net_stack::NetStackSendError>
        // where
        //     for<'de> E::Request: Deserialize<'de> + 'de,
        //     E::Response: Serialize + Clone + DeserializeOwned + 'static,
        // {
        //     loop {
        //         let req = self.inner.recv().await;
        //         let hdr = req.hdr.clone();
        //         let Some(body) = req.try_access() else {
        //             continue;
        //         };
        //         let Ok(body) = body else {
        //             continue;
        //         };
        //         let resp = f(&body.t).await;

        //         // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
        //         let hdr: ergot_base::Header = ergot_base::Header {
        //             src: {
        //                 // modify the port to match our specific port, in case the dst was port 0
        //                 let mut src = hdr.dst;
        //                 src.port_id = self.port();
        //                 src
        //             },
        //             dst: hdr.src,
        //             // TODO: we never reply to an any/all, so don't include that info
        //             any_all: None,
        //             seq_no: Some(hdr.seq_no),
        //             kind: ergot_base::FrameKind::ENDPOINT_RESP,
        //             ttl: ergot_base::DEFAULT_TTL,
        //         };
        //         return self.inner.stack().send_ty::<E::Response>(&hdr, &resp);
        //     }
        // }

        // /// Wait for an incoming packet, and respond using the given blocking closure
        // pub async fn serve_blocking<'x, F: FnOnce(&'x E::Request) -> E::Response>(
        //     &mut self,
        //     f: F,
        // ) -> Result<(), ergot_base::net_stack::NetStackSendError>
        // where
        //     E::Request: Deserialize<'x> + 'x,
        //     E::Response: Serialize + Clone + DeserializeOwned + 'static,
        // {
        //     loop {
        //         let req = self.inner.recv().await;
        //         let hdr = req.hdr.clone();
        //         let Some(body) = req.try_access() else {
        //             continue;
        //         };
        //         let Ok(body) = body else {
        //             continue;
        //         };
        //         let resp = f(&body.t);

        //         // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
        //         let hdr: ergot_base::Header = ergot_base::Header {
        //             src: {
        //                 // modify the port to match our specific port, in case the dst was port 0
        //                 let mut src = hdr.dst;
        //                 src.port_id = self.port();
        //                 src
        //             },
        //             dst: hdr.src,
        //             // TODO: we never reply to an any/all, so don't include that info
        //             any_all: None,
        //             seq_no: Some(hdr.seq_no),
        //             kind: ergot_base::FrameKind::ENDPOINT_RESP,
        //             ttl: ergot_base::DEFAULT_TTL,
        //         };
        //         return self.inner.stack().send_ty::<E::Response>(&hdr, &resp);
        //     }
        // }
    }
}

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

/// Endpoint Client/Server sockets using [`std_bounded::Bounded`](base::socket::owned::std_bounded::Bounded) storage
#[cfg(feature = "std")]
pub mod std_bounded {
    use ergot_base::{net_stack::NetStackHandle, socket::owned::std_bounded::Bounded};

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
