#[cfg(feature = "tokio-std")]
use crate::well_known::{SocketQuery, SocketQueryResponseAddress};
use crate::{net_stack::NetStackHandle, well_known::DeviceInfo};

/// A proxy type usable for performing Discovery services
pub struct Discovery<NS: NetStackHandle> {
    #[allow(dead_code)]
    pub(super) inner: NS,
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub struct DeviceRecord {
    pub addr: crate::Address,
    pub info: DeviceInfo,
}

impl<NS: NetStackHandle> Discovery<NS> {
    /// Discover devices on the network
    ///
    /// Terminates when the timeout is reached
    #[cfg(feature = "tokio-std")]
    pub async fn discover(&self, bound: usize, timeout: std::time::Duration) -> Vec<DeviceRecord> {
        use crate::{
            net_stack::topics::Topics,
            well_known::{ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoTopic},
        };

        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .heap_bounded_receiver::<ErgotDeviceInfoTopic>(bound, None);
        let subber = std::pin::pin!(subber);
        let mut hdl = subber.subscribe_unicast();
        let port = hdl.port();
        let mut rxd = vec![];

        // AFTER creating the subscription, send the interrogation
        let res = topics
            .clone()
            .broadcast_with_src_port::<ErgotDeviceInfoInterrogationTopic>(&(), None, port);
        if res.is_err() {
            return vec![];
        }

        let fut = async {
            loop {
                let msg = hdl.recv().await;
                let addr = msg.hdr.src;
                let info = msg.t;
                rxd.push(DeviceRecord { addr, info });
            }
        };
        _ = tokio::time::timeout(timeout, fut).await;

        rxd
    }

    /// Send a request to discover sockets. This sends a broadcast message with the given
    /// query parameters, then listens for responses. These requests are usually handled by
    /// `Services::socket_query_handler()`.
    ///
    /// TODO: In the future, we should have helpers like `discover_topic_socket` and
    /// `discover_endpoint_socket` that populate the `SocketQuery` with correct info.
    #[cfg(feature = "tokio-std")]
    pub async fn discover_sockets(
        &self,
        bound: usize,
        timeout: std::time::Duration,
        query: &SocketQuery,
    ) -> Vec<SocketQueryResponseAddress> {
        use crate::{
            net_stack::topics::Topics,
            well_known::{ErgotSocketQueryResponseTopic, ErgotSocketQueryTopic},
        };

        // Set up listener for responses
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .heap_bounded_receiver::<ErgotSocketQueryResponseTopic>(bound, None);
        let subber = std::pin::pin!(subber);
        // Responses are topic messages, but unicast not broadcast
        let mut hdl = subber.subscribe_unicast();
        let port = hdl.port();
        let mut rxd = vec![];

        // AFTER creating the subscription, send the interrogation
        let res = topics
            .clone()
            .broadcast_with_src_port::<ErgotSocketQueryTopic>(query, None, port);
        if res.is_err() {
            return vec![];
        }

        let fut = async {
            loop {
                let msg = hdl.recv().await;
                let mut addr = msg.hdr.src;
                addr.port_id = msg.t.port;
                rxd.push(SocketQueryResponseAddress {
                    name: msg.t.name,
                    address: addr,
                });
            }
        };
        _ = tokio::time::timeout(timeout, fut).await;

        rxd
    }
}
