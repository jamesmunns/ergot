#![doc = include_str!("../README.md")]

pub use ergot_base;

pub use ergot_base::Address;
pub use ergot_base::interface_manager;

pub mod net_stack;
pub mod socket;
pub mod well_known;

pub use net_stack::NetStack;
