//! defmt frame based logging
//!
//! This module provides support for sending raw defmt frames over the ergot
//! network. Unlike [`fmtlog`](super::fmtlog) which sends pre-formatted strings,
//! this sends compact binary defmt frames that are decoded on the host side.
//!
//! ## When to Use defmt vs fmt Logging
//!
//! **Use `defmt` (this module) when:**
//! - You need efficient logging on bandwidth-constrained links
//! - You want minimal overhead on the embedded device
//! - You're already using defmt in your project
//! - You need structured logging with type information
//! - Multiple embedded devices share the same ergot network
//!
//! **Use `fmt` (fmtlog module) when:**
//! - You want human-readable logs immediately without host tooling
//! - Your bandwidth is sufficient for formatted strings
//! - You prefer simplicity over efficiency
//! - You're using the standard `log` crate
//!
//! ## Size Comparison Example
//!
//! ```ignore
//! // fmt: sends ~45 bytes
//! log::info!("Temperature: {} °C, Pressure: {} kPa", 23.5, 101.3);
//!
//! // defmt: sends ~15 bytes (just indices + values)
//! defmt::info!("Temperature: {} °C, Pressure: {} kPa", 23.5, 101.3);
//! ```
//!
//! ## Benefits of defmt
//!
//! - **Efficient**: String formatting happens on the host, not the device
//! - **Compact**: Only sends binary frames with string table indices (70% size reduction)
//! - **Bandwidth**: Dramatically smaller than formatted strings
//! - **Structured**: Preserves type information for rich tooling
//! - **Fast**: Minimal CPU overhead on the device
//!
//! ## Architecture
//!
//! The sender side (`ErgotDefmtTx`) captures raw defmt frames as byte slices.
//! These frames are encoded by defmt and sent over the ergot network.
//! The receiver side (`ErgotDefmtRx`) gets the raw frames and can decode
//! them using defmt-decoder and the sender's ELF file.
//!
//! ## Usage Example
//!
//! See [`defmt_v1`](super::defmt_v1) module documentation for complete examples.

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// A borrowed defmt frame for sending
///
/// Contains the raw encoded defmt frame bytes. These bytes are already
/// encoded (typically with rzcobs) by the defmt encoder and are ready
/// to be transmitted over the network.
///
/// Type-punned with [`ErgotDefmtRx`] and `ErgotDefmtRxOwned` (with the `std` feature enabled).
#[derive(Serialize, Schema, Clone)]
pub struct ErgotDefmtTx<'a> {
    /// Raw defmt frame bytes (already encoded)
    pub frame: &'a [u8],
}

/// A borrowed defmt frame for receiving
///
/// Contains the raw encoded defmt frame bytes received from the network.
/// These need to be decoded using defmt-decoder along with the sender's
/// ELF file to produce human-readable log messages.
///
/// Type-punned with [`ErgotDefmtTx`] and `ErgotDefmtRxOwned` (with the `std` feature enabled).
#[derive(Serialize, Deserialize, Schema)]
pub struct ErgotDefmtRx<'a> {
    /// Raw defmt frame bytes (encoded)
    pub frame: &'a [u8],
}

/// An owned defmt frame for receiving
///
/// Same as [`ErgotDefmtRx`] but owns the frame data, useful for
/// storing or processing frames asynchronously.
///
/// Type-punned with [`ErgotDefmtRx`] and [`ErgotDefmtTx`]
#[cfg(feature = "std")]
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct ErgotDefmtRxOwned {
    /// Raw defmt frame bytes (encoded)
    pub frame: Vec<u8>,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        traits::Topic,
        well_known::{ErgotDefmtRxTopic, ErgotDefmtTxTopic},
    };

    #[test]
    fn defmt_punning_works() {
        assert_eq!(ErgotDefmtTxTopic::TOPIC_KEY, ErgotDefmtRxTopic::TOPIC_KEY);
        #[cfg(feature = "std")]
        assert_eq!(
            crate::well_known::ErgotDefmtRxOwnedTopic::TOPIC_KEY,
            ErgotDefmtRxTopic::TOPIC_KEY
        );

        let test_frame = b"\x00\x01\x02\x03";
        let res = postcard::to_vec::<_, 128>(&ErgotDefmtTx { frame: test_frame }).unwrap();

        let res = postcard::from_bytes::<ErgotDefmtRx<'_>>(&res).unwrap();
        assert_eq!(res.frame, test_frame);
    }
}
