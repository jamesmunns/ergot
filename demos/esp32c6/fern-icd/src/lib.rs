//! fern-icd

#![no_std]

use ergot::{endpoint, topic};

// TODO: It would be nice to have some kind of negotiated fifo object for
// streaming
topic!(DriverTxAvailableTopic, bool, "fern/driver/tx_available");
endpoint!(
    DriverPutTxEndpoint,
    Frame<'a>,
    PutTxResult,
    "fern/driver/put-tx"
);

// TODO: It would be nice to have some kind of negotiated fifo object for
// streaming
topic!(DriverRxAvailableTopic, bool, "fern/driver/rx_available");
endpoint!(
    DriverGetRxEndpoint,
    (),
    GetRxResult<'a>,
    "fern/driver/get-rx"
);

// TODO: It would be nice to have a combined socket kind that was like
// "sync the latest state", sort of like embassy-sync::Signal.
topic!(
    MetadataStateChangedTopic,
    AllDriverMetadata,
    "fern/driver/metadata/changed"
);
endpoint!(
    GetMetadataStateEndpoint,
    (),
    AllDriverMetadata,
    "fern/driver/metadata/get"
);

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FifoFull;

pub type PutTxResult = Result<(), FifoFull>;
pub type GetRxResult<'a> = Option<Frame<'a>>;

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct Frame<'a> {
    pub data: &'a [u8],
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct AllDriverMetadata {
    pub capabilities: Capabilities,
    pub link_state: LinkState,
    pub hw_addr: HardwareAddress,
}

// Copy some types from embassy-net-driver

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// A description of device capabilities.
///
/// Higher-level protocols may achieve higher throughput or lower latency if they consider
/// the bandwidth or packet size limitations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema)]
pub struct Capabilities {
    /// Maximum transmission unit.
    ///
    /// The network device is unable to send or receive frames larger than the value returned
    /// by this function.
    ///
    /// For Ethernet devices, this is the maximum Ethernet frame size, including the Ethernet header (14 octets), but
    /// *not* including the Ethernet FCS (4 octets). Therefore, Ethernet MTU = IP MTU + 14.
    ///
    /// Note that in Linux and other OSes, "MTU" is the IP MTU, not the Ethernet MTU, even for Ethernet
    /// devices. This is a common source of confusion.
    ///
    /// Most common IP MTU is 1500. Minimum is 576 (for IPv4) or 1280 (for IPv6). Maximum is 9216 octets.
    pub max_transmission_unit: u32,

    /// Maximum burst size, in terms of MTU.
    ///
    /// The network device is unable to send or receive bursts large than the value returned
    /// by this function.
    ///
    /// If `None`, there is no fixed limit on burst size, e.g. if network buffers are
    /// dynamically allocated.
    pub max_burst_size: Option<u32>,

    /// Checksum behavior.
    ///
    /// If the network device is capable of verifying or computing checksums for some protocols,
    /// it can request that the stack not do so in software to improve performance.
    pub checksum: ChecksumCapabilities,
}

impl From<embassy_net_driver::Capabilities> for Capabilities {
    fn from(value: embassy_net_driver::Capabilities) -> Self {
        Self {
            max_transmission_unit: value.max_transmission_unit.try_into().unwrap_or(u32::MAX),
            max_burst_size: value
                .max_burst_size
                .map(|val| val.try_into().unwrap_or(u32::MAX)),
            checksum: value.checksum.into(),
        }
    }
}

/// A description of checksum behavior for every supported protocol.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema)]
pub struct ChecksumCapabilities {
    /// Checksum behavior for IPv4.
    pub ipv4: Checksum,
    /// Checksum behavior for UDP.
    pub udp: Checksum,
    /// Checksum behavior for TCP.
    pub tcp: Checksum,
    /// Checksum behavior for ICMPv4.
    pub icmpv4: Checksum,
    /// Checksum behavior for ICMPv6.
    pub icmpv6: Checksum,
}

impl From<embassy_net_driver::ChecksumCapabilities> for ChecksumCapabilities {
    fn from(value: embassy_net_driver::ChecksumCapabilities) -> Self {
        Self {
            ipv4: value.ipv4.into(),
            udp: value.udp.into(),
            tcp: value.tcp.into(),
            icmpv4: value.icmpv4.into(),
            icmpv6: value.icmpv6.into(),
        }
    }
}

impl From<ChecksumCapabilities> for embassy_net_driver::ChecksumCapabilities {
    fn from(value: ChecksumCapabilities) -> Self {
        let mut out = Self::default();
        out.ipv4 = value.ipv4.into();
        out.udp = value.udp.into();
        out.tcp = value.tcp.into();
        out.icmpv4 = value.icmpv4.into();
        out.icmpv6 = value.icmpv6.into();
        out
    }
}

/// A description of checksum behavior for a particular protocol.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema, Default)]
pub enum Checksum {
    /// Verify checksum when receiving and compute checksum when sending.
    #[default]
    Both,
    /// Verify checksum when receiving.
    Rx,
    /// Compute checksum before sending.
    Tx,
    /// Ignore checksum completely.
    None,
}

impl From<embassy_net_driver::Checksum> for Checksum {
    fn from(value: embassy_net_driver::Checksum) -> Self {
        match value {
            embassy_net_driver::Checksum::Both => Checksum::Both,
            embassy_net_driver::Checksum::Rx => Checksum::Rx,
            embassy_net_driver::Checksum::Tx => Checksum::Tx,
            embassy_net_driver::Checksum::None => Checksum::None,
        }
    }
}

impl From<Checksum> for embassy_net_driver::Checksum {
    fn from(value: Checksum) -> Self {
        match value {
            Checksum::Both => embassy_net_driver::Checksum::Both,
            Checksum::Rx => embassy_net_driver::Checksum::Rx,
            Checksum::Tx => embassy_net_driver::Checksum::Tx,
            Checksum::None => embassy_net_driver::Checksum::None,
        }
    }
}

/// The link state of a network device.
#[derive(PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum LinkState {
    /// The link is down.
    Down,
    /// The link is up.
    Up,
}

impl From<embassy_net_driver::LinkState> for LinkState {
    fn from(value: embassy_net_driver::LinkState) -> Self {
        match value {
            embassy_net_driver::LinkState::Down => LinkState::Down,
            embassy_net_driver::LinkState::Up => LinkState::Up,
        }
    }
}

impl From<LinkState> for embassy_net_driver::LinkState {
    fn from(value: LinkState) -> Self {
        match value {
            LinkState::Down => embassy_net_driver::LinkState::Down,
            LinkState::Up => embassy_net_driver::LinkState::Up,
        }
    }
}

/// Representation of an hardware address, such as an Ethernet address or an IEEE802.15.4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum HardwareAddress {
    /// Ethernet medium, with a A six-octet Ethernet address.
    ///
    /// Devices of this type send and receive Ethernet frames,
    /// and interfaces using it must do neighbor discovery via ARP or NDISC.
    ///
    /// Examples of devices of this type are Ethernet, WiFi (802.11), Linux `tap`, and VPNs in tap (layer 2) mode.
    Ethernet([u8; 6]),
    /// 6LoWPAN over IEEE802.15.4, with an eight-octet address.
    Ieee802154([u8; 8]),
    /// Indicates that a Driver is IP-native, and has no hardware address.
    ///
    /// Devices of this type send and receive IP frames, without an
    /// Ethernet header. MAC addresses are not used, and no neighbor discovery (ARP, NDISC) is done.
    ///
    /// Examples of devices of this type are the Linux `tun`, PPP interfaces, VPNs in tun (layer 3) mode.
    Ip,
}

#[derive(Debug, PartialEq)]
pub struct NonExhaustiveError;

impl TryFrom<embassy_net_driver::HardwareAddress> for HardwareAddress {
    type Error = NonExhaustiveError;

    fn try_from(value: embassy_net_driver::HardwareAddress) -> Result<Self, Self::Error> {
        match value {
            embassy_net_driver::HardwareAddress::Ethernet(e) => Ok(HardwareAddress::Ethernet(e)),
            embassy_net_driver::HardwareAddress::Ieee802154(i) => {
                Ok(HardwareAddress::Ieee802154(i))
            }
            embassy_net_driver::HardwareAddress::Ip => Ok(HardwareAddress::Ip),
            _ => Err(NonExhaustiveError),
        }
    }
}

impl From<HardwareAddress> for embassy_net_driver::HardwareAddress {
    fn from(value: HardwareAddress) -> Self {
        match value {
            HardwareAddress::Ethernet(e) => embassy_net_driver::HardwareAddress::Ethernet(e),
            HardwareAddress::Ieee802154(i) => embassy_net_driver::HardwareAddress::Ieee802154(i),
            HardwareAddress::Ip => embassy_net_driver::HardwareAddress::Ip,
        }
    }
}
