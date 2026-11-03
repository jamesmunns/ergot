use embassy_net_0_9::udp;

#[path = "worker.rs"]
// Instantiate the shared worker with this version's socket/error types.
#[allow(clippy::duplicate_mod)]
mod worker;

pub use worker::*;
