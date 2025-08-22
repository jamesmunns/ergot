#[cfg(not(feature = "std"))]
use crate::well_known::ErgotDeviceInfoTopic;
#[cfg(feature = "std")]
use crate::{
    fmtlog::ErgotFmtRxOwned,
    socket::HeaderMessage,
    well_known::{ErgotDeviceInfoOwnedTopic, ErgotFmtRxOwnedTopic, OwnedDeviceInfo},
};
use crate::{
    net_stack::{NetStackHandle, endpoints::Endpoints, topics::Topics},
    well_known::{DeviceInfo, ErgotDeviceInfoInterrogationTopic, ErgotPingEndpoint},
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
    pub async fn device_info_handler<const D: usize>(self, info: &DeviceInfo<'_>) -> ! {
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .bounded_receiver::<ErgotDeviceInfoInterrogationTopic, D>(None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe();
        // TODO: This is a hack.
        //
        // There is a limitation right now where OWNED recievers cannot receive borrowed
        // messages, because there is no code path that allows for the ser->de round trip
        // that would be required. This is NORMALLY fine if the message actually transits
        // through an interface, because it will be serialized and deserialized. But for
        // testing, we might want to discover ourselves, which causes a mismatch because
        // we're sending borrowed but receiving owned.
        //
        // This also affects fmt logging, but we haven't added this hack there yet.
        //
        // It might be worth adding the ability to ser+deser on std using a vec/boxed slice
        // as a scratch buffer, but that's for another day.
        //
        // We could also turn this into a borrowing topic receiver, but we need to resolve
        // some functionality there first.
        #[cfg(feature = "std")]
        let info = OwnedDeviceInfo {
            name: info.name.map(|s| s.to_string()),
            description: info.description.map(|s| s.to_string()),
            unique_id: info.unique_id,
        };
        #[cfg(feature = "std")]
        let info = &info;

        loop {
            let msg = hdl.recv().await;
            let dest = msg.hdr.src;

            // Same hack as above: on std, send as an owned message
            #[cfg(not(feature = "std"))]
            let _ = topics
                .clone()
                .unicast_borrowed::<ErgotDeviceInfoTopic>(dest, info);
            #[cfg(feature = "std")]
            let _ = topics
                .clone()
                .unicast::<ErgotDeviceInfoOwnedTopic>(dest, info);
        }
    }

    /// Handler for log messages that calls the given function for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn generic_log_handler<F>(self, depth: usize, f: F) -> !
    where
        F: Fn(HeaderMessage<ErgotFmtRxOwned>),
    {
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
