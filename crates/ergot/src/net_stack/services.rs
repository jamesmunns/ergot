#[cfg(feature = "std")]
use crate::{fmtlog::ErgotFmtRxOwned, socket::HeaderMessage};
use crate::{
    net_stack::{NetStackHandle, endpoints::Endpoints, topics::Topics},
    well_known::{
        DeviceInfo, ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoTopic, ErgotPingEndpoint,
    },
};
use core::pin::pin;

/// A proxy type usable for creating helper services
pub struct Services<NS: NetStackHandle> {
    pub(super) inner: NS,
}

impl<NS: NetStackHandle> Services<NS> {
    /// Automatically responds to direct pings via the [`ErgotPingEndpoint`] endpoint
    ///
    /// The const parameter `D` controls the depth of the socket to buffer ping requests
    pub async fn ping_handler<const D: usize>(self) -> ! {
        let server = Endpoints {
            inner: self.inner.clone(),
        }
        .bounded_server::<ErgotPingEndpoint, D>(None);
        let server = pin!(server);
        let mut server_hdl = server.attach();
        loop {
            _ = server_hdl.serve_blocking(u32::clone).await;
        }
    }

    /// Handler for device info requests
    ///
    /// The const parameter `D` controls the depth of the socket to buffer info requests
    pub async fn device_info_handler<const D: usize>(self, info: &DeviceInfo) -> ! {
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .bounded_receiver::<ErgotDeviceInfoInterrogationTopic, D>(None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe();

        loop {
            let msg = hdl.recv().await;
            let dest = msg.hdr.src;

            let _ = topics.clone().unicast::<ErgotDeviceInfoTopic>(dest, info);
        }
    }

    /// Handler for log messages that calls the given function for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn generic_log_handler<F>(self, depth: usize, f: F) -> !
    where
        F: Fn(HeaderMessage<ErgotFmtRxOwned>),
    {
        use crate::well_known::ErgotFmtRxOwnedTopic;

        let subber = Topics {
            inner: self.inner.clone(),
        }
        .heap_bounded_receiver::<ErgotFmtRxOwnedTopic>(depth, None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe();
        loop {
            let msg = hdl.recv().await;
            f(msg)
        }
    }

    /// Handler for log messages that prints to the `log` crate sink for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn log_handler(self, depth: usize) -> ! {
        use crate::fmtlog;

        self.generic_log_handler(depth, |msg| match msg.t.level {
            fmtlog::Level::Error => log::error!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Warn => log::warn!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Info => log::info!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Debug => log::debug!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Trace => log::trace!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
        })
        .await
    }

    /// Handler for log messages that prints to stdout for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn default_stdout_log_handler(self, depth: usize) -> ! {
        self.generic_log_handler(depth, |msg| {
            println!(
                "({}.{}:{}) {:?}: {}",
                msg.hdr.src.network_id,
                msg.hdr.src.node_id,
                msg.hdr.src.port_id,
                msg.t.level,
                msg.t.inner,
            );
        })
        .await
    }
}
