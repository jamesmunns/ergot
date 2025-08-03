use crate::{endpoint, topic};
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};
#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;

endpoint!(ErgotPingEndpoint, u32, u32, "ergot/.well-known/ping");
topic!(ErgotFmtTxTopic, ErgotFmtTx<'a>, "ergot/.well-known/fmt");
topic!(ErgotFmtRxTopic, ErgotFmtRx<'a>, "ergot/.well-known/fmt");

#[cfg(feature = "std")]
topic!(ErgotFmtRxOwnedTopic, ErgotFmtRxOwned, "ergot/.well-known/fmt");
