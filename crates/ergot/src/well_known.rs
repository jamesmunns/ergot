use postcard_schema::Schema;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::fmtlog::{ErgotFmtRx, ErgotFmtTx};

#[cfg(all(feature = "defmtlog", feature = "std"))]
use crate::logging::defmtlog::ErgotDefmtRxOwned;
#[cfg(feature = "defmtlog")]
use crate::logging::defmtlog::{ErgotDefmtRx, ErgotDefmtTx};

use crate::interface_manager::{
    AddressClaimError, AddressRefreshError, FragmentPacketError, FragmentRequestError,
    NodeClaimAssignment, SeedAssignmentError, SeedNetAssignment, SeedRefreshError,
};
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
#[cfg(feature = "defmtlog")]
topic!(
    ErgotDefmtTxTopic,
    ErgotDefmtTx<'a>,
    "ergot/.well-known/defmt"
);
#[cfg(feature = "defmtlog")]
topic!(
    ErgotDefmtRxTopic,
    ErgotDefmtRx<'a>,
    "ergot/.well-known/defmt"
);

#[cfg(all(feature = "defmtlog", feature = "std"))]
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
pub type SeedRouterReleaseResponse = Result<(), SeedRefreshError>;
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
endpoint!(
    ErgotSeedRouterReleaseEndpoint,
    SeedRouterReleaseRequest,
    SeedRouterReleaseResponse,
    "ergot/.well-known/seed-router/release"
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
    "ergot/.well-known/address/claim"
);
endpoint!(
    ErgotAddressRefreshEndpoint,
    AddressRefreshRequest,
    AddressRefreshResponse,
    "ergot/.well-known/address/refresh"
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

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
pub struct FragmentRequest {
    pub complete_size: u16,
    pub packet_data_size: u16,
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
pub struct FragmentRequestResponse {
    pub buffer_id: u8,
    pub port: u8,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Schema, Clone, PartialEq)]
pub struct FragmentPacket<const SIZE: usize> {
    pub buffer_id: u8,
    pub packet_idx: u16,
    #[serde_as(as = "[_; SIZE]")]
    pub data: [u8; SIZE],
}

type FragmentRequestResponseResult = Result<FragmentRequestResponse, FragmentRequestError>;
type FragmentPacketResponse = Result<(), FragmentPacketError>;

endpoint!(
    ErgotFragmentRequestEndpoint,
    FragmentRequest,
    FragmentRequestResponseResult,
    "ergot/.well-known/fragment/request"
);

// Implemented by hand since the [`endpoint!`] macro doesn't like const generics
pub struct ErgotFragmentPacketEndpoint<const SIZE: usize> {
    _priv: core::marker::PhantomData<()>,
}
impl<const SIZE: usize> crate::traits::Endpoint for ErgotFragmentPacketEndpoint<SIZE> {
    type Request = FragmentPacket<SIZE>;
    type Response = FragmentPacketResponse;
    const PATH: &'static str = "ergot/.well-known/fragment/packet";
    const REQ_KEY: crate::traits::Key =
        crate::traits::Key::for_path::<FragmentPacket<SIZE>>("ergot/.well-known/fragment/packet");
    const RESP_KEY: crate::traits::Key =
        crate::traits::Key::for_path::<FragmentPacketResponse>("ergot/.well-known/fragment/packet");
}
