//! The Null Profile
//!
//! This is the simplest profile, and does not handle external connections at all.
//!
//! Useful for testing, or cases where only local communication is desirable.

use serde::Serialize;

use crate::{
    Header,
    interface_manager::{ConstInit, InterfaceSendError, InterfaceState, Profile, SetStateError},
};

/// The null profile
pub struct Null {
    _priv: (),
}

impl ConstInit for Null {
    const INIT: Self = Self { _priv: () };
}

impl Profile for Null {
    type InterfaceIdent = ();

    fn send<T: Serialize>(&mut self, hdr: &Header, _data: &T) -> Result<(), InterfaceSendError> {
        if hdr.dst.net_node_any() {
            Err(InterfaceSendError::DestinationLocal)
        } else {
            Err(InterfaceSendError::NoRouteToDest)
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        _hdr_raw: &[u8],
        _data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        if hdr.dst.net_node_any() {
            Err(InterfaceSendError::DestinationLocal)
        } else {
            Err(InterfaceSendError::NoRouteToDest)
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        _err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        if hdr.dst.net_node_any() {
            Err(InterfaceSendError::DestinationLocal)
        } else {
            Err(InterfaceSendError::NoRouteToDest)
        }
    }

    fn interface_state(&mut self, _ident: Self::InterfaceIdent) -> Option<InterfaceState> {
        None
    }

    fn set_interface_state(
        &mut self,
        _ident: Self::InterfaceIdent,
        _state: InterfaceState,
    ) -> Result<(), crate::interface_manager::SetStateError> {
        Err(SetStateError::InterfaceNotFound)
    }
}
