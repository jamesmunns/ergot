#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(feature = "std", test)), no_std)]

pub use ergot_base;

pub use ergot_base::Address;
pub use ergot_base::interface_manager;
pub use ergot_base::exports;

pub mod book;
pub mod net_stack;
pub mod socket;
pub mod traits;
pub mod well_known;
pub mod toolkits;

pub use net_stack::NetStack;
