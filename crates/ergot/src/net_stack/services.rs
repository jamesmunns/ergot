use crate::{net_stack::NetStackHandle, well_known::ErgotPingEndpoint};
use core::pin::pin;

/// A proxy type usable for creating helper services
pub struct Services<NS: NetStackHandle> {
    pub(super) inner: NS,
}

impl<NS: NetStackHandle> Services<NS> {
    /// Automatically responds to direct pings via the [`ErgotPingEndpoint`] endpoint
    pub async fn ping_handler<const D: usize>(&self) -> ! {
        let stack = self.inner.stack();
        let server = stack.stack_bounded_endpoint_server::<ErgotPingEndpoint, D>(None);
        let server = pin!(server);
        let mut server_hdl = server.attach();
        loop {
            _ = server_hdl.serve_blocking(u32::clone).await;
        }
    }
}
