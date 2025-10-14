pub mod fmtlog;
pub mod log_v0_4;

// conditional logging re-exports

#[allow(unused_imports)]
#[cfg(feature = "defmt-v1")]
pub(crate) use defmt::{debug, error, info, trace, warn};

#[allow(unused_imports)]
#[cfg(not(feature = "defmt-v1"))]
pub(crate) use log::{debug, error, info, trace, warn};
