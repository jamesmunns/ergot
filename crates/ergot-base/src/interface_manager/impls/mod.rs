//! Interface Manager Implementations
//!
//! We provide a number of Interface Manager implementations that are usable for common
//! cases.
//!
//! The simplest of these is [`null`], which never routes messages externally, and can
//! be used in cases where you only want intra-app messaging, or for testing.
//!
//! These interface managers are generally one of three configurations:
//!
//! * "seed routers" that can connect with many devices, and assign network IDs
//! * "bridges" that have the ability to connect multiple interfaces
//! * "edge" nodes, that have a single interface
//!
//! Other implementations require enabling features. These include:
//!
//! * `eusb_0_4_client` - An "edge" configuration for connecting an embassy firmware
//!   over USB bulk packets. Requires the `embassy-usb-v0_4` feature.
//! * `eusb_0_5_client` - An "edge" configuration for connecting an embassy firmware
//!   over USB bulk packets. Requires the `embassy-usb-v0_5` feature.
//! * `nusb_0_1_router` - A "seed router" that scans and automatically connects to
//!   usb devices using the `eusb` client implementations. Requires the `nusb-v0_1` feature.
//! * `std_tcp_router` - A "seed router" that allows devices to connect over a TCP
//!   based interface. Requires the `std` feature.
//! * `std_tcp_client` - An "edge" configuration that can connect to a `std_tcp_router`.
//!   Requires the `std` feature.

#[cfg(feature = "embassy-usb-v0_4")]
pub mod eusb_0_4_client;
#[cfg(feature = "embassy-usb-v0_5")]
pub mod eusb_0_5_client;
pub mod null;
#[cfg(feature = "nusb-v0_1")]
pub mod nusb_0_1_router;
#[cfg(feature = "std")]
pub mod std_tcp_client;
#[cfg(feature = "std")]
pub mod std_tcp_router;
