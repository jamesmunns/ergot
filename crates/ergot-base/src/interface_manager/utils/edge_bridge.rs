//! Edge Bridge is the simplest case of a "Bridge" profile device
//!
//! It can connect an "upstream" network segment to a "downstream" network
//! segment. HOWEVER: the "downstream" network segment must ONLY be edge
//! nodes.
//!
//! This does NOT allow downstream devices to also act as bridges, as this
//! implementation assumes it will never hear packets from the "downstream"
//! interface that are not the specific net id assigned to it.
//!
//! This is fine:
//!
//! ```text
//! ┌───────────┐   ┌───────────┐   ┌───────────┐
//! │ Upstream  │◀─▶│Edge Bridge│◀─▶│ Edge Node │
//! └───────────┘   └───────────┘   └───────────┘
//! ```
//!
//! This is also fine:
//! ```text
//! ┌───────────┐   ┌───────────┐
//! │ Upstream  │◀─▶│Edge Bridge│───────┐
//! └───────────┘   └───────────┘       │
//!                       ┌─────────────┼─────────────┐
//!                       ▼             ▼             ▼
//!                 ┌───────────┐ ┌───────────┐ ┌───────────┐
//!                 │ Edge Node │ │ Edge Node │ │ Edge Node │
//!                 └───────────┘ └───────────┘ └───────────┘
//! ```
//!
//! This is NOT allowed:
//!
//! ```text
//! ┌───────────┐   ┌───────────┐   ┌───────────┐   ┌───────────┐
//! │ Upstream  │◀─▶│Edge Bridge│◀─▶│Edge Bridge│◀─▶│ Edge Node │
//! └───────────┘   └───────────┘   └───────────┘   └───────────┘
//! ```

pub struct EdgeBridge {}
