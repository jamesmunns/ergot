//! Embassy-net UDP workers with version-specific socket types.
//!
//! Select `v0_7` or `v0_9` when both dependency versions are enabled.
//! The unversioned worker exports retain the 0.7 API when that feature is
//! enabled; otherwise they use 0.9. The versioned toolkits also export their
//! matching workers.

use crate::wire_frames::MAX_HDR_ENCODED_SIZE;

pub const UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX: usize = 1500 - 8 - 20;
pub const UDP_OVER_ETH_ERGOT_PAYLOAD_SIZE_MAX: usize =
    UDP_OVER_ETH_ERGOT_FRAME_SIZE_MAX - MAX_HDR_ENCODED_SIZE;

#[cfg(feature = "embassy-net-v0_7")]
pub mod v0_7;
#[cfg(feature = "embassy-net-v0_9")]
pub mod v0_9;

#[cfg(feature = "embassy-net-v0_7")]
pub use v0_7::{RxTxError, RxTxWorker, SocketAlreadyActive};
#[cfg(all(feature = "embassy-net-v0_9", not(feature = "embassy-net-v0_7")))]
pub use v0_9::{RxTxError, RxTxWorker, SocketAlreadyActive};
