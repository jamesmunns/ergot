use core::{marker::PhantomData, pin::pin};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Address, DEFAULT_TTL, FrameKind, Header, HeaderSeq, socket::HeaderMessage, traits::Endpoint,
};

use super::{NetStackHandle, NetStackSendError, ReqRespError};

/// A proxy type usable for creating helper services
#[derive(Clone)]
pub struct Endpoints<NS: NetStackHandle> {
    pub(super) inner: NS,
}

pub struct EndpointClient<'a, E: Endpoint, NS: NetStackHandle> {
    inner: NS,
    name: Option<&'a str>,
    address: Address,
    _pd: PhantomData<fn() -> E>,
}

impl<E, NS> EndpointClient<'_, E, NS>
where
    E: Endpoint,
    NS: NetStackHandle,
{
    pub async fn request(&self, req: &E::Request) -> Result<E::Response, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        let ep = Endpoints {
            inner: self.inner.clone(),
        };
        ep.request::<E>(self.address, req, self.name).await
    }
}

impl<NS: NetStackHandle> Endpoints<NS> {
    pub fn client<E: Endpoint>(
        self,
        address: Address,
        name: Option<&str>,
    ) -> EndpointClient<'_, E, NS> {
        EndpointClient {
            inner: self.inner,
            _pd: PhantomData,
            name,
            address,
        }
    }

    /// Perform an [`Endpoint`] Request, and await Response.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
    /// # use ergot::NetStack;
    /// # use ergot::interface_manager::profiles::null::Null;
    /// use ergot::Address;
    /// // Define an example endpoint
    /// ergot::endpoint!(Example, u32, i32, "pathho");
    ///
    /// static STACK: NetStack<CSRMutex, Null> = NetStack::new();
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     // (not shown: starting an `Example` service...)
    ///     # let jhdl = tokio::task::spawn(async {
    ///     #     println!("Serve!");
    ///     #     let srv = STACK.endpoints().bounded_server::<Example, 16>(None);
    ///     #     let srv = core::pin::pin!(srv);
    ///     #     let mut hdl = srv.attach();
    ///     #     hdl.serve(async |p| *p as i32).await.unwrap();
    ///     #     println!("Served!");
    ///     # });
    ///     # // TODO: let the server attach first
    ///     # tokio::task::yield_now().await;
    ///     # tokio::time::sleep(core::time::Duration::from_millis(50)).await;
    ///     // Make a ping request to local
    ///     let res = STACK.endpoints().request::<Example>(
    ///         Address::unknown(),
    ///         &42u32,
    ///         None,
    ///     ).await;
    ///     assert_eq!(res, Ok(42i32));
    ///     # jhdl.await.unwrap();
    /// }
    /// ```
    pub async fn request<E>(
        self,
        dst: Address,
        req: &E::Request,
        name: Option<&str>,
    ) -> Result<E::Response, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        let resp = self.request_full::<E>(dst, req, name).await?;
        Ok(resp.t)
    }

    /// Same as [`Self::request`], but also returns the full message with header
    pub async fn request_full<E>(
        self,
        dst: Address,
        req: &E::Request,
        name: Option<&str>,
    ) -> Result<HeaderMessage<E::Response>, ReqRespError>
    where
        E: Endpoint,
        E::Request: Serialize + Clone + DeserializeOwned + 'static,
        E::Response: Serialize + Clone + DeserializeOwned + 'static,
    {
        // Response doesn't need a name because we will reply back.
        //
        // We can also use a "single"/oneshot response because we know
        // this request will get exactly one response.
        let resp_sock = self.clone().single_client::<E>();
        let resp_sock = pin!(resp_sock);
        let mut resp_hdl = resp_sock.attach();

        resp_hdl.send_request(dst, req, name)?;
        // TODO: assert seq nos match somewhere? do we NEED seq nos if we have
        // port ids now?
        let resp = resp_hdl.recv().await;
        match resp {
            Ok(msg) => Ok(msg),
            Err(e) => Err(ReqRespError::Remote(e.t)),
        }
    }

    /// Send an endpoint response. Useful if you used `recv_manual()` and need to make a manual
    /// response.
    pub fn respond_owned<E>(
        self,
        req_hdr: &HeaderSeq,
        resp: &E::Response,
    ) -> Result<(), NetStackSendError>
    where
        E: Endpoint,
        E::Response: Serialize + Clone + 'static,
    {
        // NOTE: We swap src/dst, AND we go from req -> resp (both in kind and key)
        let hdr: Header = Header {
            src: req_hdr.dst,
            dst: req_hdr.src,
            any_all: None,
            seq_no: Some(req_hdr.seq_no),
            kind: FrameKind::ENDPOINT_RESP,
            ttl: DEFAULT_TTL,
        };
        self.inner.stack().send_ty::<E::Response>(&hdr, resp)
    }

    pub fn single_client<E: Endpoint>(self) -> crate::socket::endpoint::single::Client<E, NS>
    where
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::single::Client::new(self.inner, None)
    }

    pub fn single_server<E: Endpoint>(
        self,
        name: Option<&str>,
    ) -> crate::socket::endpoint::single::Server<E, NS>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::single::Server::new(self.inner, name)
    }

    pub fn bounded_server<E: Endpoint, const N: usize>(
        self,
        name: Option<&str>,
    ) -> crate::socket::endpoint::stack_vec::Server<E, NS, N>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::stack_vec::Server::new(self.inner, name)
    }

    #[cfg(feature = "std")]
    pub fn heap_bounded_server<E: Endpoint>(
        self,
        bound: usize,
        name: Option<&str>,
    ) -> crate::socket::endpoint::std_bounded::Server<E, NS>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
        E::Response: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::endpoint::std_bounded::Server::new(self.inner, bound, name)
    }
}
