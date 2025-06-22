use serde::Serialize;

use crate::Header;

use super::{ConstInit, InterfaceManager, InterfaceSendError};

pub struct NullInterfaceManager {
    _priv: (),
}

impl ConstInit for NullInterfaceManager {
    const INIT: Self = Self { _priv: () };
}

impl InterfaceManager for NullInterfaceManager {
    fn send<T: Serialize>(&mut self, hdr: &Header, _data: &T) -> Result<(), InterfaceSendError> {
        if hdr.dst.net_node_any() {
            Err(InterfaceSendError::DestinationLocal)
        } else {
            Err(InterfaceSendError::PlaceholderOhNo)
        }
    }

    fn send_raw(&mut self, hdr: &Header, _data: &[u8]) -> Result<(), InterfaceSendError> {
        if hdr.dst.net_node_any() {
            Err(InterfaceSendError::DestinationLocal)
        } else {
            Err(InterfaceSendError::PlaceholderOhNo)
        }
    }
}
