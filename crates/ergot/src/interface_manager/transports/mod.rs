//! Transport-agnostic RxWorker implementations.
//!
//! Each transport (embedded-io, USB bulk, etc.) is implemented once and
//! works with any profile via the [`FrameProcessor`] trait.
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor

pub mod packet;

#[cfg(any(feature = "embedded-io-async-v0_6", feature = "embedded-io-async-v0_7"))]
pub mod eio;

#[cfg(feature = "embassy-usb-v0_5")]
pub mod eusb_0_5;

#[cfg(feature = "embassy-usb-v0_6")]
pub mod eusb_0_6;

#[cfg(feature = "tokio-std")]
pub mod tokio_cobs_stream;

#[cfg(feature = "tokio-std")]
pub mod tokio_udp;

#[cfg(feature = "tokio-serial-v5")]
pub mod tokio_serial;

#[cfg(feature = "nusb-v0_1")]
pub mod nusb;

#[cfg(feature = "embassy-net-v0_7")]
pub mod embassy_net_udp;
