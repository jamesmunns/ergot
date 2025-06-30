use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Endpoint;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base::{self as base, socket::Response};

pub mod single {
    use super::*;
    use ergot_base::{socket::{single, Attributes}, FrameKind};

    #[pin_project]
    pub struct EndpointReqSocket<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: single::Socket<E::Request, R, M>,
    }

    #[pin_project]
    pub struct EndpointRespSocket<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: single::Socket<E::Response, R, M>,
    }

    pub struct EndpointReqSocketHdl<'a, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: single::SocketHdl<'a, E::Request, R, M>,
    }

    pub struct EndpointRespSocketHdl<'a, E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: single::SocketHdl<'a, E::Response, R, M>,
    }

    impl<E, R, M> EndpointReqSocket<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: single::Socket::new(
                    &net.inner,
                    base::Key(E::REQ_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> EndpointReqSocketHdl<'a, E, R, M> {
            let this = self.project();
            let hdl: single::SocketHdl<'_, E::Request, R, M> = this.sock.attach();
            EndpointReqSocketHdl { hdl }
        }
    }

    impl<E, R, M> EndpointReqSocketHdl<'_, E, R, M>
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


    impl<E, R, M> EndpointRespSocket<E, R, M>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
            Self {
                sock: single::Socket::new(
                    &net.inner,
                    base::Key(E::RESP_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_RESP,
                        discoverable: false,
                    },
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> EndpointRespSocketHdl<'a, E, R, M> {
            let this = self.project();
            let hdl: single::SocketHdl<'_, E::Response, R, M> = this.sock.attach();
            EndpointRespSocketHdl { hdl }
        }
    }

    impl<E, R, M> EndpointRespSocketHdl<'_, E, R, M>
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
    use ergot_base::{socket::Attributes, FrameKind};

    use super::*;

    #[pin_project]
    pub struct EndpointSocket<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        #[pin]
        sock: base::socket::std_bounded::Socket<E::Request, R, M>,
    }

    impl<E, R, M> EndpointSocket<E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub fn new(stack: &'static crate::NetStack<R, M>, bound: usize) -> Self {
            Self {
                sock: base::socket::std_bounded::Socket::new(
                    &stack.inner,
                    base::Key(E::REQ_KEY.to_bytes()),
                    Attributes {
                        kind: FrameKind::ENDPOINT_REQ,
                        discoverable: true,
                    },
                    bound,
                ),
            }
        }

        pub fn attach<'a>(self: Pin<&'a mut Self>) -> EndpointSocketHdl<'a, E, R, M> {
            let this = self.project();
            let hdl: base::socket::std_bounded::SocketHdl<'_, E::Request, R, M> = this.sock.attach();
            EndpointSocketHdl { hdl }
        }
    }

    pub struct EndpointSocketHdl<'a, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        hdl: base::socket::std_bounded::SocketHdl<'a, E::Request, R, M>,
    }

    impl<E, R, M> EndpointSocketHdl<'_, E, R, M>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        R: ScopedRawMutex + 'static,
        M: InterfaceManager + 'static,
    {
        pub async fn recv_manual(&mut self) -> Response<E::Request> {
            self.hdl.recv().await
        }

        pub async fn serve<F: AsyncFnOnce(E::Request) -> E::Response>(
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
            let resp = f(t).await;
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

}
