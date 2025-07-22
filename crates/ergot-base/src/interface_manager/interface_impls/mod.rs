#[cfg(feature = "std")]
pub mod std_tcp;

#[cfg(any(feature = "embassy-usb-v0_4", feature = "embassy-usb-v0_5"))]
pub mod embassy_usb;
