//! Transport layer implementations for ergot
//!
//! This module provides embedded-io-async compatible wrappers for various
//! transport protocols.

#[cfg(all(
    feature = "rtt",
    any(feature = "embedded-io-async-v0_6", feature = "embedded-io-async-v0_7")
))]
pub mod rtt;
