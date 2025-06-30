use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Endpoint;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base::{self as base, socket::Response};

pub mod raw {
    use super::*;
    use ergot_base::{
        FrameKind,
        socket::{
            Attributes,
            raw::{self, Storage},
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
        sock: raw::Socket<S, E::Request, R, M>,
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
        sock: raw::Socket<S, E::Response, R, M>,
    }

    pub struct ServerHandle<'a, S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: raw::SocketHdl<'a, S, E::Request, R, M>,
    }

    pub struct ClientHandle<'a, S, E, R, M>
    where
        S: Storage<Response<E::Response>>,
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: raw::SocketHdl<'a, S, E::Response, R, M>,
    }

    impl<S, E, R, M> Server<S, E, R, M>
    where
        S: Storage<Response<E::Request>>,
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>, sto: S) -> Self {
            Self {
                sock: raw::Socket::new(
                    &net.inner,
                    base::Key(E::REQ_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                    sto,
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, S, E, R, M> {
            let this = self.project();
            let hdl: raw::SocketHdl<'_, S, E::Request, R, M> = this.sock.attach();
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
            let base::socket::OwnedMessage { hdr, t } = msg;
            let resp = f(&t).await;

            // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
            let hdr: base::Header = base::Header {
                src: hdr.dst,
                dst: hdr.src,
                key: Some(base::Key(E::RESP_KEY.to_bytes())),
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
        pub const fn new(net: &'static crate::NetStack<R, M>, sto: S) -> Self {
            Self {
                sock: raw::Socket::new(
                    &net.inner,
                    base::Key(E::RESP_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_RESP,
                        discoverable: false,
                    },
                    sto,
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, S, E, R, M> {
            let this = self.project();
            let hdl: raw::SocketHdl<'_, S, E::Response, R, M> = this.sock.attach();
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

pub mod single {
    use super::*;

    #[pin_project]
    pub struct Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::Server<Option<Response<E::Request>>, E, R, M>,
    }

    #[pin_project]
    pub struct Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::Client<Option<Response<E::Response>>, E, R, M>,
    }

    pub struct ServerHandle<'a, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::ServerHandle<'a, Option<Response<E::Request>>, E, R, M>,
    }

    pub struct ClientHandle<'a, E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::ClientHandle<'a, Option<Response<E::Response>>, E, R, M>,
    }

    impl<E, R, M> Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: super::raw::Server::new(net, None),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, E, R, M> {
            let this = self.project();
            let hdl: super::raw::ServerHandle<'_, _, _, R, M> = this.sock.attach();
            ServerHandle { hdl }
        }
    }

    impl<E, R, M> ServerHandle<'_, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

        pub async fn recv_manual(&mut self) -> Response<E::Request> {
            self.hdl.recv_manual().await
        }

        pub async fn serve<F: AsyncFnOnce(&E::Request) -> E::Response>(
            &mut self,
            f: F,
        ) -> Result<(), base::net_stack::NetStackSendError>
        where
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
        {
            self.hdl.serve(f).await
        }
    }

    impl<E, R, M> Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: super::raw::Client::new(net, None),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, E, R, M> {
            let this = self.project();
            let hdl: super::raw::ClientHandle<'_, _, _, R, M> = this.sock.attach();
            ClientHandle { hdl }
        }
    }

    impl<E, R, M> ClientHandle<'_, E, R, M>
    where
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

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

pub mod std_bounded {
    use ergot_base::socket::std_bounded::Bounded;

    use super::*;
    #[pin_project]
    pub struct Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::Server<Bounded<Response<E::Request>>, E, R, M>,
    }

    #[pin_project]
    pub struct Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: super::raw::Client<Bounded<Response<E::Response>>, E, R, M>,
    }

    pub struct ServerHandle<'a, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::ServerHandle<'a, Bounded<Response<E::Request>>, E, R, M>,
    }

    pub struct ClientHandle<'a, E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: super::raw::ClientHandle<'a, Bounded<Response<E::Response>>, E, R, M>,
    }

    impl<E, R, M> Server<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: super::raw::Server::new(net, Bounded::with_bound(bound)),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ServerHandle<'a, E, R, M> {
            let this = self.project();
            let hdl: super::raw::ServerHandle<'_, _, _, R, M> = this.sock.attach();
            ServerHandle { hdl }
        }
    }

    impl<E, R, M> ServerHandle<'_, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn port(&self) -> u8 {
            self.hdl.port()
        }

        pub async fn recv_manual(&mut self) -> Response<E::Request> {
            self.hdl.recv_manual().await
        }

        pub async fn serve<F: AsyncFnOnce(&E::Request) -> E::Response>(
            &mut self,
            f: F,
        ) -> Result<(), base::net_stack::NetStackSendError>
        where
            E::Response: Serialize + Clone + DeserializeOwned + 'static,
        {
            self.hdl.serve(f).await
        }
    }

    impl<E, R, M> Client<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(net: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: super::raw::Client::new(net, Bounded::with_bound(bound)),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> ClientHandle<'a, E, R, M> {
            let this = self.project();
            let hdl: super::raw::ClientHandle<'_, _, _, R, M> = this.sock.attach();
            ClientHandle { hdl }
        }
    }

    impl<E, R, M> ClientHandle<'_, E, R, M>
    where
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
