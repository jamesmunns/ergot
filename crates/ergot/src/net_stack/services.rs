#[cfg(feature = "std")]
use crate::{
    fmtlog::ErgotFmtRxOwned, net_stack::topics::Topics, socket::HeaderMessage,
    well_known::ErgotFmtRxOwnedTopic,
};
use crate::{
    net_stack::{NetStackHandle, endpoints::Endpoints},
    well_known::ErgotPingEndpoint,
};
use core::pin::pin;

/// A proxy type usable for creating helper services
pub struct Services<NS: NetStackHandle> {
    pub(super) inner: NS,
}

impl<NS: NetStackHandle> Services<NS> {
    /// Automatically responds to direct pings via the [`ErgotPingEndpoint`] endpoint
    pub async fn ping_handler<const D: usize>(&self) -> ! {
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

    #[cfg(feature = "std")]
    pub async fn generic_log_handler<F>(&self, depth: usize, f: F) -> !
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

    #[cfg(feature = "std")]
    pub async fn default_stdout_log_handler(&self, depth: usize) -> ! {
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
