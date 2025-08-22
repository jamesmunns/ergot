use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};
use crate::{endpoint, topic};

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
    (),
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

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct DeviceInfo<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub unique_id: u64,
}

#[cfg(feature = "std")]
#[derive(Debug, Serialize, Deserialize, Schema, Clone, Hash, PartialEq, Eq)]
pub struct OwnedDeviceInfo {
    pub name: Option<String>,
    pub description: Option<String>,
    pub unique_id: u64,
}
