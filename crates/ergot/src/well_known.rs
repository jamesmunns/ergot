use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};
use crate::{Address, endpoint, topic};

endpoint!(ErgotPingEndpoint, u32, u32, "ergot/.well-known/ping");
topic!(ErgotFmtTxTopic, ErgotFmtTx<'a>, "ergot/.well-known/fmt");
topic!(ErgotFmtRxTopic, ErgotFmtRx<'a>, "ergot/.well-known/fmt");
topic!(
    ErgotDeviceInfoTopic,
    DeviceInfo<'a>,
    "ergot/.well-known/device-info"
);
topic!(
    ErgotDeviceInfoInterrogationTopic,
    Address,
    "ergot/.well-known/device-info/interrogation"
);

#[cfg(feature = "std")]
topic!(
    ErgotFmtRxOwnedTopic,
    ErgotFmtRxOwned,
    "ergot/.well-known/fmt"
);
#[cfg(feature = "std")]
topic!(
    ErgotDeviceInfoOwnedTopic,
    OwnedDeviceInfo,
    "ergot/.well-known/device-info"
);

#[derive(Debug, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct DeviceInfo<'a> {
    name: Option<&'a str>,
    description: Option<&'a str>,
    unique_id: u64,
}

#[cfg(feature = "std")]
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct OwnedDeviceInfo {
    name: Option<String>,
    description: Option<String>,
    unique_id: u64,
}
