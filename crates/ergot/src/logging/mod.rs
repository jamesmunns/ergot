//! Here we make logging macros available.
//! Based on features we either export defmt macros, log wrapper macros, or no-ops.
//!
//! - `defmt-v1-internal`: ergot internal logs use defmt (INCOMPATIBLE with defmt-sink-network)
//! - `log-internal` or `std`: ergot internal logs use log/geil()
//! - neither: ergot internal logs are silent (no-op)

#![allow(unused_macros)]

pub mod fmtlog;
pub mod log_v0_4;

// defmt message types and topics for network-based defmt logging
// (needed by both senders and receivers of defmt frames over ergot network)
#[cfg(feature = "defmtlog")]
pub mod defmtlog;

// defmt global_logger implementation
// (only needed if you want ergot to act as the defmt global_logger)
#[cfg(any(feature = "defmt-sink-network", feature = "defmt-sink-rtt"))]
pub mod defmt_sink;

// Compile error: defmt-v1-internal + defmt-sink-network = feedback loop
// Note: _all-features-hack is excluded so `cargo clippy --all-features` works in CI
#[cfg(all(
    feature = "defmt-v1-internal",
    feature = "defmt-sink-network",
    not(feature = "_all-features-hack")
))]
compile_error!(
    "Features `defmt-v1-internal` and `defmt-sink-network` are incompatible: \
     ergot's internal defmt logs would feed back into the network sink, causing an infinite loop. \
     Use `log-internal` instead for ergot internal logging with defmt-sink-network."
);

// ============================================================================
// Mode 1: defmt-v1-internal (no-std only) — ergot internals use defmt
// ============================================================================
#[allow(unused_imports)]
#[cfg(all(feature = "defmt-v1-internal", not(feature = "std")))]
pub(crate) use defmt::{debug, error, info, trace, warn};

// ============================================================================
// Mode 2: log-internal or std — ergot internals use log/geil()
// ============================================================================
#[allow(unused_macros)]
#[clippy::format_args]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
macro_rules! debug {
    (target: $target:expr, $($arg:tt)+) => ({
        log::debug!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });
    ($($arg:tt)+) => (log::debug!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_macros)]
#[clippy::format_args]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
macro_rules! error {
    (target: $target:expr, $($arg:tt)+) => ({
        log::error!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });
    ($($arg:tt)+) => (log::error!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_macros)]
#[clippy::format_args]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
macro_rules! info {
    (target: $target:expr, $($arg:tt)+) => ({
        log::info!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });
    ($($arg:tt)+) => (log::info!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_macros)]
#[clippy::format_args]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
macro_rules! trace {
    (target: $target:expr, $($arg:tt)+) => ({
        log::trace!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });
    ($($arg:tt)+) => (log::trace!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_macros)]
#[clippy::format_args]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
macro_rules! warni {
    (target: $target:expr, $($arg:tt)+) => ({
        log::warn!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });
    ($($arg:tt)+) => (log::warn!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_imports)]
#[cfg(any(
    feature = "log-internal",
    all(feature = "std", not(feature = "defmt-v1-internal"))
))]
pub(crate) use {debug, error, info, trace, warni as warn};

// ============================================================================
// Mode 3: no internal logging — silent no-ops
// ============================================================================
#[allow(unused_macros)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
macro_rules! debug {
    ($($arg:tt)*) => {};
}

#[allow(unused_macros)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
macro_rules! error {
    ($($arg:tt)*) => {};
}

#[allow(unused_macros)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
macro_rules! info {
    ($($arg:tt)*) => {};
}

#[allow(unused_macros)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}

#[allow(unused_macros)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
macro_rules! warni {
    ($($arg:tt)*) => {};
}

#[allow(unused_imports)]
#[cfg(not(any(
    all(feature = "defmt-v1-internal", not(feature = "std")),
    feature = "log-internal",
    feature = "std",
)))]
pub(crate) use {debug, error, info, trace, warni as warn};
