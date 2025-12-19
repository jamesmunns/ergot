//! Here we make logging macros available.
//! Based on features we either export defmt macros or log wrapper macros.

#![allow(unused_macros)]

pub mod fmtlog;
pub mod log_v0_4;

// conditional logging re-exports

#[allow(unused_imports)]
#[cfg(all(feature = "defmt-v1", not(feature = "std")))]
pub(crate) use defmt::{debug, error, info, trace, warn};

/// Wrapper macro for log::debug that uses the internal ergot logger.
#[clippy::format_args]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
macro_rules! debug {
    // debug!(target: "my_target", key1 = 42, key2 = true; "a {} event", "log")
    // debug!(target: "my_target", "a {} event", "log")
    (target: $target:expr, $($arg:tt)+) => ({
        log::debug!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });

    // debug!("a {} event", "log")
    ($($arg:tt)+) => (log::debug!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

/// Wrapper macro for log::error that uses the internal ergot logger.
#[clippy::format_args]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
macro_rules! error {
    // error!(target: "my_target", key1 = 42, key2 = true; "a {} event", "log")
    // error!(target: "my_target", "a {} event", "log")
    (target: $target:expr, $($arg:tt)+) => ({
        log::error!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });

    // error!("a {} event", "log")
    ($($arg:tt)+) => (log::error!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

/// Wrapper macro for log::info that uses the internal ergot logger.
#[clippy::format_args]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
macro_rules! info {
    // info!(target: "my_target", key1 = 42, key2 = true; "a {} event", "log")
    // info!(target: "my_target", "a {} event", "log")
    (target: $target:expr, $($arg:tt)+) => ({
        log::info!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });

    // info!("a {} event", "log")
    ($($arg:tt)+) => (log::info!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

/// Wrapper macro for log::trace that uses the internal ergot logger.
#[clippy::format_args]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
macro_rules! trace {
    // trace!(target: "my_target", key1 = 42, key2 = true; "a {} event", "log")
    // trace!(target: "my_target", "a {} event", "log")
    (target: $target:expr, $($arg:tt)+) => ({
        log::trace!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });

    // trace!("a {} event", "log")
    ($($arg:tt)+) => (log::trace!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

/// Wrapper macro for log::warn that uses the internal ergot logger.
#[clippy::format_args]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
macro_rules! warni {
    // warn!(target: "my_target", key1 = 42, key2 = true; "a {} event", "log")
    // warn!(target: "my_target", "a {} event", "log")
    (target: $target:expr, $($arg:tt)+) => ({
        log::warn!(logger: $crate::logging::log_v0_4::internal::geil(), target: $target, $($arg)+)
    });

    // warn!("a {} event", "log")
    ($($arg:tt)+) => (log::warn!(logger: $crate::logging::log_v0_4::internal::geil(), $($arg)+))
}

#[allow(unused_imports)]
#[cfg(not(all(feature = "defmt-v1", not(feature = "std"))))]
pub(crate) use {debug, error, info, trace, warni as warn};
