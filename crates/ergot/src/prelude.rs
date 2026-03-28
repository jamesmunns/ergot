//! Convenience re-exports for common ergot types.
//!
//! Power users who need full control over `Router` generics, multi-interface
//! enums, and custom transports can pull in most of what they need with a
//! single wildcard import:
//!
//! ```rust,ignore
//! use ergot::prelude::*;
//! ```

// Core types
pub use crate::address::Address;
pub use crate::net_stack::{NetStack, NetStackHandle, NetStackSendError};
pub use crate::{FrameKind, Header, HeaderSeq, Key, ProtocolError};

#[cfg(feature = "std")]
pub use crate::net_stack::ArcNetStack;

// Interface manager essentials
pub use crate::interface_manager::{Interface, InterfaceSink, InterfaceState, Profile};

#[cfg(feature = "std")]
pub use crate::interface_manager::LivenessConfig;

// Profiles
pub use crate::interface_manager::profiles::direct_edge::{
    CENTRAL_NODE_ID, DirectEdge, EDGE_NODE_ID, EdgeFrameProcessor,
};

#[cfg(any(feature = "std", feature = "nostd-seed-router"))]
pub use crate::interface_manager::profiles::router::{Router, RouterFrameProcessor};
