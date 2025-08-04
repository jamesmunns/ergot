//! Interface Implementations
//!
//! Interfaces are the "wire format" of ergot. They determine how messages are handled between
//! two devices. Interfaces are typically held by the Profile used by a netstack.

#[cfg(feature = "std")]
pub mod std_tcp;

#[cfg(feature = "tokio-serial-v5")]
pub mod std_serial_cobs;

#[cfg(any(feature = "embassy-usb-v0_4", feature = "embassy-usb-v0_5"))]
pub mod embassy_usb;

#[cfg(feature = "nusb-v0_1")]
pub mod nusb_bulk;

#[cfg(feature = "embedded-io-async-v0_6")]
pub mod embedded_io;
