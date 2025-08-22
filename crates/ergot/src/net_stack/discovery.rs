use crate::net_stack::NetStackHandle;

#[cfg(feature = "tokio-std")]
use crate::{
    net_stack::topics::Topics,
    well_known::{ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoOwnedTopic, OwnedDeviceInfo},
};

/// A proxy type usable for performing Discovery services
pub struct Discovery<NS: NetStackHandle> {
    #[allow(dead_code)]
    pub(super) inner: NS,
}

#[cfg(feature = "tokio-std")]
#[derive(Debug, Hash, PartialEq, Eq)]
pub struct DeviceRecord {
    pub addr: crate::Address,
    pub info: OwnedDeviceInfo,
}

impl<NS: NetStackHandle> Discovery<NS> {
    /// Discover devices on the network
    ///
    /// Terminates when the timeout is reached
    #[cfg(feature = "tokio-std")]
    pub async fn discover(&self, bound: usize, timeout: std::time::Duration) -> Vec<DeviceRecord> {
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .heap_bounded_receiver::<ErgotDeviceInfoOwnedTopic>(bound, None);
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
}
