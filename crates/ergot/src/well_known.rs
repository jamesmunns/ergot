use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};

#[cfg(all(feature = "defmt-v1", feature = "std"))]
use crate::logging::defmtlog::ErgotDefmtRxOwned;
#[cfg(feature = "defmt-v1")]
use crate::logging::defmtlog::{ErgotDefmtRx, ErgotDefmtTx};

use crate::interface_manager::{SeedAssignmentError, SeedNetAssignment, SeedRefreshError};
use crate::nash::NameHash;
use crate::{Address, FrameKind, endpoint, topic};

endpoint!(ErgotPingEndpoint, u32, u32, "ergot/.well-known/ping");

// Formatted string logging topics
topic!(ErgotFmtTxTopic, ErgotFmtTx<'a>, "ergot/.well-known/fmt");
topic!(ErgotFmtRxTopic, ErgotFmtRx<'a>, "ergot/.well-known/fmt");

#[cfg(feature = "std")]
topic!(
    ErgotFmtRxOwnedTopic,
    ErgotFmtRxOwned,
    "ergot/.well-known/fmt"
);

// defmt frame logging topics
#[cfg(feature = "defmt-v1")]
topic!(ErgotDefmtTxTopic, ErgotDefmtTx<'a>, "ergot/.well-known/defmt");
#[cfg(feature = "defmt-v1")]
topic!(ErgotDefmtRxTopic, ErgotDefmtRx<'a>, "ergot/.well-known/defmt");

#[cfg(all(feature = "defmt-v1", feature = "std"))]
topic!(
    ErgotDefmtRxOwnedTopic,
    ErgotDefmtRxOwned,
    "ergot/.well-known/defmt"
);

// Device info topics
topic!(
    ErgotDeviceInfoTopic,
    DeviceInfo,
    "ergot/.well-known/device-info"
);
topic!(
    ErgotDeviceInfoInterrogationTopic,
    (),
    "ergot/.well-known/device-info/interrogation"
);

topic!(
    ErgotSocketQueryTopic,
    SocketQuery,
    "ergot/.well-known/socket/query"
);
topic!(
    ErgotSocketQueryResponseTopic,
    SocketQueryResponse,
    "ergot/.well-known/socket/query/response"
);

pub type SeedRouterAssignmentResponse = Result<SeedRouterAssignment, SeedAssignmentError>;
pub type SeedRouterRefreshResponse = Result<SeedNetAssignment, SeedRefreshError>;
endpoint!(
    ErgotSeedRouterAssignmentEndpoint,
    (),
    SeedRouterAssignmentResponse,
    "ergot/.well-known/seed-router/request"
);
endpoint!(
    ErgotSeedRouterRefreshEndpoint,
    SeedRouterRefreshRequest,
    SeedRouterRefreshResponse,
    "ergot/.well-known/seed-router/refresh"
);

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct DeviceInfo {
    pub name: Option<heapless::String<16>>,
    pub description: Option<heapless::String<32>>,
    pub unique_id: u64,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub enum NameRequirement {
    None,
    Any,
    Specific(NameHash),
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SocketQuery {
    pub key: [u8; 8],
    pub nash_req: NameRequirement,
    pub frame_kind: FrameKind,
    pub broadcast: bool,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SocketQueryResponseAddress {
    pub name: Option<NameHash>,
    pub address: Address,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SocketQueryResponse {
    pub name: Option<NameHash>,
    pub port: u8,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SeedRouterAssignment {
    pub assignment: SeedNetAssignment,
    pub refresh_port: u8,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SeedRouterRefreshRequest {
    pub refresh_net: u16,
    pub refresh_token: [u8; 8],
}
