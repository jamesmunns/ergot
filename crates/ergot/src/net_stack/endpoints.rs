use core::{marker::PhantomData, pin::pin};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, nash::NameHash,
    socket::HeaderMessage, traits::Endpoint,
};

use super::{NetStackHandle, ReqRespError};

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
    ) -> EndpointClient<E, NS> {
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
    /// use ergot::socket::endpoint::std_bounded::Server;
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
    ///     #     let srv = STACK.endpoints().heap_bounded_server::<Example>(16, None);
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
        let stack = self.inner.stack();
        let resp_sock = self.clone().single_client::<E>();
        let resp_sock = pin!(resp_sock);
        let mut resp_hdl = resp_sock.attach();

        // If the destination is wildcard, include the any_all appendix to the
        // header
        let any_all = match dst.port_id {
            0 => Some(AnyAllAppendix {
                key: Key(E::REQ_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            255 => {
                return Err(ReqRespError::NoBroadcast);
            }
            _ => None,
        };

        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: resp_hdl.port(),
            },
            dst,
            any_all,
            seq_no: None,
            kind: FrameKind::ENDPOINT_REQ,
            ttl: DEFAULT_TTL,
        };
        stack.send_ty(&hdr, req).map_err(ReqRespError::Local)?;
        // TODO: assert seq nos match somewhere? do we NEED seq nos if we have
        // port ids now?
        let resp = resp_hdl.recv().await;
        match resp {
            Ok(msg) => Ok(msg),
            Err(e) => Err(ReqRespError::Remote(e.t)),
        }
    }

    pub fn single_client<E: Endpoint>(self) -> crate::socket::endpoint::single::Client<E, NS>
    where
        E::Request: Serialize + DeserializeOwned + Clone,
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
