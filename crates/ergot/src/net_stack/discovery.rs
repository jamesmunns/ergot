#[cfg(all(feature = "std", time_sleep))]
use core::pin::pin;

#[cfg(all(feature = "std", time_sleep))]
use crate::{
    logging::debug,
    net_stack::topics::Topics,
    time::{Duration, with_timeout},
    well_known::{
        ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoTopic, ErgotSocketQueryResponseTopic,
        ErgotSocketQueryTopic, SocketQuery, SocketQueryResponseAddress,
    },
};
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
    /// Terminates when the timeout is reached. Needs `std` and a [time
    /// backend](crate::time).
    #[cfg(all(feature = "std", time_sleep))]
    pub async fn discover(&self, bound: usize, timeout: Duration) -> Vec<DeviceRecord> {
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .heap_bounded_receiver::<ErgotDeviceInfoTopic>(bound, None);
        let subber = pin!(subber);
        let mut hdl = subber.subscribe_unicast();
        let port = hdl.port();
        let mut rxd = vec![];

        // AFTER creating the subscription, send the interrogation. Broadcasting
        // is at-most-once and to an empty network it is a successful no-op (no
        // error to short-circuit on), so just listen for whatever responds
        // within the timeout — with no interface the listener simply times out
        // and returns an empty result. A genuine send failure (e.g. a full
        // interface queue) also just waits out the timeout, but keep it
        // observable.
        if let Err(e) = topics
            .clone()
            .broadcast_with_src_port::<ErgotDeviceInfoInterrogationTopic>(&(), None, port)
        {
            debug!("discovery interrogation broadcast failed: {:?}", e);
        }

        let fut = async {
            loop {
                let msg = hdl.recv().await;
                let addr = msg.hdr.src;
                let info = msg.t;
                rxd.push(DeviceRecord { addr, info });
            }
        };
        _ = with_timeout(timeout, fut).await;

        rxd
    }

    /// Send a request to discover sockets. This sends a broadcast message with the given
    /// query parameters, then listens for responses. These requests are usually handled by
    /// `Services::socket_query_handler()`.
    ///
    /// TODO: In the future, we should have helpers like `discover_topic_socket` and
    /// `discover_endpoint_socket` that populate the `SocketQuery` with correct info.
    ///
    /// Needs `std` and a [time backend](crate::time).
    #[cfg(all(feature = "std", time_sleep))]
    pub async fn discover_sockets(
        &self,
        bound: usize,
        timeout: Duration,
        query: &SocketQuery,
    ) -> Vec<SocketQueryResponseAddress> {
        // Set up listener for responses
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .heap_bounded_receiver::<ErgotSocketQueryResponseTopic>(bound, None);
        let subber = pin!(subber);
        // Responses are topic messages, but unicast not broadcast
        let mut hdl = subber.subscribe_unicast();
        let port = hdl.port();
        let mut rxd = vec![];

        // AFTER creating the subscription, send the query. Best-effort
        // broadcast — see `discover` for the at-most-once rationale.
        if let Err(e) = topics
            .clone()
            .broadcast_with_src_port::<ErgotSocketQueryTopic>(query, None, port)
        {
            debug!("socket query broadcast failed: {:?}", e);
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
        _ = with_timeout(timeout, fut).await;

        rxd
    }
}
