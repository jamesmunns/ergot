//! "Edge" device profile
//!
//! Edge devices are the second simplest device profile, and are intended for devices
//! that are on the "edge" of a network, e.g. they have a single upstream connection
//! to a bridge or seed router.
//!
//! These devices use as many tricks as possible to be as simple as possible. They
//! initially start not knowing their network ID, and if a packet is sent to them,
//! they assume the destination net ID is their net ID. They will also blindly send
//! any outgoing packets, rather than trying to determine whether that packet is
//! actually routable to a node on the network.

use log::{debug, trace};
use serde::Serialize;

#[cfg(feature = "embassy-usb-v0_4")]
pub mod eusb_0_4;

#[cfg(feature = "embassy-usb-v0_5")]
pub mod eusb_0_5;

#[cfg(feature = "std")]
pub mod std_tcp;

use crate::{
    Header, ProtocolError,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceSink, InterfaceState, Profile, SetStateError,
    },
    wire_frames::CommonHeader,
};

pub const CENTRAL_NODE_ID: u8 = 1;
pub const EDGE_NODE_ID: u8 = 2;

pub enum SetNetIdError {
    CantSetZero,
    NoActiveSink,
}

// TODO: call this something like "point to point edge"
pub struct DirectEdge<I: Interface> {
    sink: I::Sink,
    seq_no: u16,
    state: InterfaceState,
    own_node_id: u8,
    other_node_id: u8,
}

impl<I: Interface> DirectEdge<I> {
    pub const fn new_target(sink: I::Sink) -> Self {
        Self {
            sink,
            seq_no: 0,
            state: InterfaceState::Down,
            own_node_id: EDGE_NODE_ID,
            other_node_id: CENTRAL_NODE_ID,
        }
    }

    pub const fn new_controller(sink: I::Sink, state: InterfaceState) -> Self {
        Self {
            sink,
            seq_no: 0,
            state,
            own_node_id: CENTRAL_NODE_ID,
            other_node_id: EDGE_NODE_ID,
        }
    }
}

impl<I: Interface> DirectEdge<I> {
    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut I::Sink, CommonHeader), InterfaceSendError> {
        let net_id = match &self.state {
            InterfaceState::Down | InterfaceState::Inactive => {
                return Err(InterfaceSendError::NoRouteToDest);
            }
            InterfaceState::ActiveLocal { .. } => {
                // TODO: maybe also handle this?
                return Err(InterfaceSendError::NoRouteToDest);
            }
            InterfaceState::Active { net_id, node_id: _ } => *net_id,
        };

        trace!("common_send header: {:?}", ihdr);

        if net_id == 0 {
            debug!("Attempted to send via interface before we have been assigned a net ID");
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        if ihdr.dst.network_id == net_id && ihdr.dst.node_id == self.own_node_id {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Now that we've filtered out "dest local" checks, see if there is
        // any TTL left before we send to the next hop
        let mut hdr = ihdr.clone();
        hdr.decrement_ttl()?;

        // If the source is local, rewrite the source using this interface's
        // information so responses can find their way back here
        if hdr.src.net_node_any() {
            // todo: if we know the destination is EXACTLY this network,
            // we could leave the network_id local to allow for shorter
            // addresses
            hdr.src.network_id = net_id;
            hdr.src.node_id = self.own_node_id;
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = net_id;
            hdr.dst.node_id = self.other_node_id;
        }

        let seq_no = self.seq_no;
        self.seq_no = self.seq_no.wrapping_add(1);

        let header = CommonHeader {
            src: hdr.src,
            dst: hdr.dst,
            seq_no,
            kind: hdr.kind,
            ttl: hdr.ttl,
        };
        if [0, 255].contains(&hdr.dst.port_id) && ihdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        Ok((&mut self.sink, header))
    }
}

impl<I: Interface> Profile for DirectEdge<I> {
    type InterfaceIdent = ();

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_ty(&header, hdr.any_all.as_ref(), data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_raw(&header, hdr_raw, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn interface_state(&mut self, _ident: ()) -> Option<InterfaceState> {
        Some(self.state)
    }

    fn set_interface_state(
        &mut self,
        _ident: (),
        state: InterfaceState,
    ) -> Result<(), SetStateError> {
        match state {
            InterfaceState::Down => {
                self.state = InterfaceState::Down;
            }
            InterfaceState::Inactive => {
                self.state = InterfaceState::Inactive;
            }
            InterfaceState::ActiveLocal { node_id } => {
                if node_id != self.own_node_id {
                    return Err(SetStateError::InvalidNodeId);
                }
                self.state = InterfaceState::ActiveLocal { node_id };
            }
            InterfaceState::Active { net_id, node_id } => {
                if node_id != self.own_node_id {
                    return Err(SetStateError::InvalidNodeId);
                }
                self.state = InterfaceState::Active { net_id, node_id };
            }
        }
        Ok(())
    }
}
