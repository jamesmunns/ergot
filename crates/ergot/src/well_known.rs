#[cfg(feature = "tokio-std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};
use crate::{endpoint, topic};

endpoint!(ErgotPingEndpoint, u32, u32, "ergot/.well-known/ping");
topic!(ErgotFmtTxTopic, ErgotFmtTx<'a>, "ergot/.well-known/fmt");
topic!(ErgotFmtRxTopic, ErgotFmtRx<'a>, "ergot/.well-known/fmt");

#[cfg(feature = "tokio-std")]
topic!(
    ErgotFmtRxOwnedTopic,
    ErgotFmtRxOwned,
    "ergot/.well-known/fmt"
);

pub mod handlers {
    use crate::NetStack;
    use crate::interface_manager::Profile;
    use core::pin::pin;
    use mutex::ScopedRawMutex;

    use super::ErgotPingEndpoint;

    /// Automatically responds to direct pings via the [`ErgotPingEndpoint`] endpoint
    pub async fn ping_handler<R, M, const D: usize>(stack: &NetStack<R, M>) -> !
    where
        R: ScopedRawMutex,
        M: Profile,
    {
        let server = stack.stack_bounded_endpoint_server::<ErgotPingEndpoint, D>(None);
        let server = pin!(server);
        let mut server_hdl = server.attach();
        loop {
            _ = server_hdl.serve_blocking(u32::clone).await;
        }
    }
}
