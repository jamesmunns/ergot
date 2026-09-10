use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};

#[cfg(all(feature = "defmtlog", feature = "std"))]
use crate::logging::defmtlog::ErgotDefmtRxOwned;
#[cfg(feature = "defmtlog")]
use crate::logging::defmtlog::{ErgotDefmtRx, ErgotDefmtTx};

use crate::interface_manager::{
    AddressClaimError, AddressRefreshError, NodeClaimAssignment, SeedAssignmentError,
    SeedNetAssignment, SeedRefreshError,
};
use crate::nash::NameHash;
use crate::{Address, FrameKind, endpoint, topic};

endpoint!(
    ErgotPingEndpoint,
    u32,
    u32,
    "ergot/.well-known/ping",
    class = crate::TrafficClass::Control
);

// Formatted string logging topics
topic!(
    ErgotFmtTxTopic,
    ErgotFmtTx<'a>,
    "ergot/.well-known/fmt",
    class = crate::TrafficClass::Background
);
topic!(
    ErgotFmtRxTopic,
    ErgotFmtRx<'a>,
    "ergot/.well-known/fmt",
    class = crate::TrafficClass::Background
);

#[cfg(feature = "std")]
topic!(
    ErgotFmtRxOwnedTopic,
    ErgotFmtRxOwned,
    "ergot/.well-known/fmt",
    class = crate::TrafficClass::Background
);

// defmt frame logging topics
#[cfg(feature = "defmtlog")]
topic!(
    ErgotDefmtTxTopic,
    ErgotDefmtTx<'a>,
    "ergot/.well-known/defmt",
    class = crate::TrafficClass::Background
);
#[cfg(feature = "defmtlog")]
topic!(
    ErgotDefmtRxTopic,
    ErgotDefmtRx<'a>,
    "ergot/.well-known/defmt",
    class = crate::TrafficClass::Background
);

#[cfg(all(feature = "defmtlog", feature = "std"))]
topic!(
    ErgotDefmtRxOwnedTopic,
    ErgotDefmtRxOwned,
    "ergot/.well-known/defmt",
    class = crate::TrafficClass::Background
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
pub type SeedRouterReleaseResponse = Result<(), SeedRefreshError>;
endpoint!(
    ErgotSeedRouterAssignmentEndpoint,
    (),
    SeedRouterAssignmentResponse,
    "ergot/.well-known/seed-router/request",
    class = crate::TrafficClass::Control
);
endpoint!(
    ErgotSeedRouterRefreshEndpoint,
    SeedRouterRefreshRequest,
    SeedRouterRefreshResponse,
    "ergot/.well-known/seed-router/refresh",
    class = crate::TrafficClass::Control
);
endpoint!(
    ErgotSeedRouterReleaseEndpoint,
    SeedRouterReleaseRequest,
    SeedRouterReleaseResponse,
    "ergot/.well-known/seed-router/release",
    class = crate::TrafficClass::Control
);

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct DeviceInfo {
    pub name: Option<heapless::String<16>>,
    pub description: Option<heapless::String<32>>,
    pub unique_id: u64,
    /// The [`WIRE_VERSION`](crate::WIRE_VERSION) this device speaks. Peers
    /// on a different version cannot exchange frames at all; this field
    /// exists so tooling can say *why* a device is silent.
    pub wire_version: u8,
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
    pub release_port: u8,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SeedRouterRefreshRequest {
    pub refresh_net: u16,
    pub refresh_token: [u8; 8],
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct SeedRouterReleaseRequest {
    pub release_net: u16,
    pub refresh_token: [u8; 8],
}

// Bus Address Claim
pub type AddressClaimResponse = Result<AddressClaimGranted, AddressClaimError>;
pub type AddressRefreshResponse = Result<NodeClaimAssignment, AddressRefreshError>;

endpoint!(
    ErgotAddressClaimEndpoint,
    AddressClaimRequest,
    AddressClaimResponse,
    "ergot/.well-known/address/claim",
    class = crate::TrafficClass::Control
);
endpoint!(
    ErgotAddressRefreshEndpoint,
    AddressRefreshRequest,
    AddressRefreshResponse,
    "ergot/.well-known/address/refresh",
    class = crate::TrafficClass::Control
);

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct AddressClaimRequest {
    pub candidate_node_id: u8,
    pub nonce: u64,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct AddressClaimGranted {
    pub assignment: NodeClaimAssignment,
    pub refresh_port: u8,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct AddressRefreshRequest {
    pub node_id: u8,
    pub refresh_token: [u8; 8],
}

// Path MTU Discovery
endpoint!(
    ErgotPathMtuEndpoint,
    PathMtuQuery,
    PathMtuResult,
    "ergot/.well-known/path-mtu"
);

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct PathMtuQuery {
    pub path_mtu: u16,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct PathMtuResult {
    pub path_mtu: u16,
}
