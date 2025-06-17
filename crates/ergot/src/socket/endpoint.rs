use std::pin::{Pin, pin};

use crate::interface_manager::InterfaceManager;
use mutex::ScopedRawMutex;
use pin_project::pin_project;
use postcard_rpc::Endpoint;
use serde::{Serialize, de::DeserializeOwned};

use ergot_base as base;

use super::{
    owned::{OwnedSocket, OwnedSocketHdl},
    std_bounded::{StdBoundedSocket, StdBoundedSocketHdl},
};

#[pin_project]
pub struct OwnedEndpointSocket<E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    sock: OwnedSocket<E::Request, R, M>,
}

impl<E, R, M> OwnedEndpointSocket<E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub const fn new(net: &'static crate::NetStack<R, M>) -> Self {
        Self {
            sock: OwnedSocket::new_endpoint_req::<E>(net),
        }
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> OwnedEndpointSocketHdl<'a, E, R, M> {
        let this = self.project();
        let hdl: OwnedSocketHdl<'_, E::Request, R, M> = this.sock.attach();
        OwnedEndpointSocketHdl { hdl }
    }
}

pub struct OwnedEndpointSocketHdl<'a, E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: OwnedSocketHdl<'a, E::Request, R, M>,
}

impl<E, R, M> OwnedEndpointSocketHdl<'_, E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub async fn recv_manual(&mut self) -> base::socket::OwnedMessage<E::Request> {
        self.hdl.recv().await
    }

    pub async fn serve<F: AsyncFnOnce(E::Request) -> E::Response>(
        &mut self,
        f: F,
    ) -> Result<(), base::net_stack::NetStackSendError>
    where
        E::Response: Serialize + DeserializeOwned + 'static,
    {
        let msg = self.hdl.recv().await;
        let base::socket::OwnedMessage { hdr, t } = msg;
        let resp = f(t).await;

        // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
        let hdr: base::Header = base::Header {
            src: hdr.dst,
            dst: hdr.src,
            key: Some(base::Key(E::RESP_KEY.to_bytes())),
            seq_no: Some(hdr.seq_no),
            kind: base::FrameKind::ENDPOINT_RESP,
        };
        self.hdl.stack().send_ty::<E::Response>(hdr, resp)
    }
}

// ---
// TODO: Do we need some kind of Socket trait we can use to dedupe things like this?

#[pin_project]
pub struct StdBoundedEndpointSocket<E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    #[pin]
    sock: StdBoundedSocket<E::Request, R, M>,
}

impl<E, R, M> StdBoundedEndpointSocket<E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub fn new(stack: &'static base::net_stack::NetStack<R, M>, bound: usize) -> Self {
        Self {
            sock: StdBoundedSocket::new_endpoint_req::<E>(stack, bound),
        }
    }

    pub fn attach<'a>(self: Pin<&'a mut Self>) -> StdBoundedEndpointSocketHdl<'a, E, R, M> {
        let this = self.project();
        let hdl: StdBoundedSocketHdl<'_, E::Request, R, M> = this.sock.attach();
        StdBoundedEndpointSocketHdl { hdl }
    }
}

pub struct StdBoundedEndpointSocketHdl<'a, E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    hdl: StdBoundedSocketHdl<'a, E::Request, R, M>,
}

impl<E, R, M> StdBoundedEndpointSocketHdl<'_, E, R, M>
where
    E: Endpoint,
    E::Request: Serialize + DeserializeOwned + 'static,
    R: ScopedRawMutex + 'static,
    M: InterfaceManager + 'static,
{
    pub async fn recv_manual(&mut self) -> base::socket::OwnedMessage<E::Request> {
        self.hdl.recv().await
    }

    pub async fn serve<F: AsyncFnOnce(E::Request) -> E::Response>(
        &mut self,
        f: F,
    ) -> Result<(), base::net_stack::NetStackSendError>
    where
        E::Response: Serialize + DeserializeOwned + 'static,
    {
        let msg = self.hdl.recv().await;
        let base::socket::OwnedMessage { hdr, t } = msg;
        let resp = f(t).await;
        // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
        let hdr: base::Header = base::Header {
            src: hdr.dst,
            dst: hdr.src,
            key: Some(base::Key(E::RESP_KEY.to_bytes())),
            seq_no: Some(hdr.seq_no),
            kind: base::FrameKind::ENDPOINT_RESP,
        };
        self.hdl.stack().send_ty::<E::Response>(hdr, resp)
    }
}
