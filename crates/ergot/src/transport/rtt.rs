//! RTT channel wrapper implementing embedded-io-async traits for ergot
//!
//! This module provides async wrappers around RTT (Real-Time Transfer) channels,
//! allowing them to be used as transport layers with the embedded-io-async traits.
//!
//! RTT is a debug protocol that uses memory-mapped ring buffers to communicate
//! between the embedded device and a debug probe (like J-Link or probe-rs).
//!
//! ## Usage
//!
//! ```ignore
//! use ergot::{rtt_target, transport::rtt::{RttReader, RttWriter}};
//!
//! // Initialize RTT channels
//! let channels = rtt_target::rtt_init! {
//!     up: {
//!         0: { size: 1024, name: "data_tx" }
//!     },
//!     down: {
//!         0: { size: 512, name: "data_rx" }
//!     }
//! };
//!
//! // Create ergot transport
//! let reader = RttReader::new(channels.down.0);
//! let writer = RttWriter::new(channels.up.0);
//!
//! // Use with ergot's interface manager
//! // ...
//! ```

#[cfg(feature = "embedded-io-async-v0_6")]
use embedded_io_async_0_6 as embedded_io_async;
#[cfg(all(
    feature = "embedded-io-async-v0_7",
    not(feature = "embedded-io-async-v0_6")
))]
use embedded_io_async_0_7 as embedded_io_async;

use embedded_io_async::{ErrorType, Read, Write};
use rtt_target::{DownChannel, UpChannel};

/// Error type for RTT I/O operations
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
pub struct RttError;

impl core::fmt::Display for RttError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RTT I/O error")
    }
}

// Implement Error for both std and no_std (requires Rust 1.81+)
#[cfg(feature = "std")]
impl std::error::Error for RttError {}

#[cfg(not(feature = "std"))]
impl core::error::Error for RttError {}

impl embedded_io_async::Error for RttError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// RTT channel wrapper for reading (DownChannel - host to device)
pub struct RttReader {
    down: &'static mut DownChannel,
}

impl RttReader {
    /// Create a new RTT reader from a down channel
    ///
    /// # Arguments
    /// * `down` - The RTT down channel (host → device)
    pub fn new(down: &'static mut DownChannel) -> Self {
        Self { down }
    }
}

impl ErrorType for RttReader {
    type Error = RttError;
}

impl Read for RttReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Non-blocking read from RTT down channel
        // If no data available, yield to prevent busy-waiting
        loop {
            let n = self.down.read(buf);
            if n > 0 {
                return Ok(n);
            }
            // Yield to allow other tasks to run
            embassy_futures::yield_now().await;
        }
    }
}

/// RTT channel wrapper for writing (UpChannel - device to host)
pub struct RttWriter {
    channel: &'static mut UpChannel,
}

impl RttWriter {
    /// Create a new RTT writer from an up channel
    ///
    /// # Arguments
    /// * `channel` - The RTT up channel (device → host)
    pub fn new(channel: &'static mut UpChannel) -> Self {
        Self { channel }
    }
}

impl ErrorType for RttWriter {
    type Error = RttError;
}

impl Write for RttWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // RTT write is blocking, but typically very fast
        let written = self.channel.write(buf);
        Ok(written)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // RTT doesn't need explicit flushing
        Ok(())
    }
}
