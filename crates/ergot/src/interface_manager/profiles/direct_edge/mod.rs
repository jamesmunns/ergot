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

use log::{debug, trace, warn};
use serde::Serialize;

#[cfg(feature = "embedded-io-async-v0_6")]
pub mod eio_0_6;

#[cfg(feature = "embassy-usb-v0_4")]
pub mod eusb_0_4;

#[cfg(feature = "embassy-usb-v0_5")]
pub mod eusb_0_5;

#[cfg(feature = "tokio-std")]
pub mod tokio_tcp;

use crate::{
    Header, HeaderSeq, ProtocolError,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceSink, InterfaceState, Profile, SetStateError,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
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
        hdr: &Header,
    ) -> Result<(&'b mut I::Sink, HeaderSeq), InterfaceSendError> {
        let net_id = match &self.state {
            InterfaceState::Down => {
                trace!("{hdr}: ignoring send, interface down");
                return Err(InterfaceSendError::NoRouteToDest);
            }
            InterfaceState::Inactive => {
                trace!("{hdr}: ignoring send, interface inactive");
                return Err(InterfaceSendError::NoRouteToDest);
            }
            InterfaceState::ActiveLocal { .. } => {
                // TODO: maybe also handle this?
                trace!("{hdr}: ignoring send, interface local only");
                return Err(InterfaceSendError::NoRouteToDest);
            }
            InterfaceState::Active { net_id, node_id: _ } => *net_id,
        };

        trace!("{hdr}: common_send");

        if net_id == 0 {
            debug!("Attempted to send via interface before we have been assigned a net ID");
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        if hdr.dst.network_id == net_id && hdr.dst.node_id == self.own_node_id {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Now that we've filtered out "dest local" checks, see if there is
        // any TTL left before we send to the next hop
        let mut hdr = hdr.clone();
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

        let header = hdr.to_headerseq_or_with_seq(|| {
            let seq_no = self.seq_no;
            self.seq_no = self.seq_no.wrapping_add(1);
            seq_no
        });
        if [0, 255].contains(&hdr.dst.port_id) && hdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        Ok((&mut self.sink, header))
    }
}

impl<I: Interface> Profile for DirectEdge<I> {
    type InterfaceIdent = ();

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_ty(&header, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        if source.is_some() {
            return Err(InterfaceSendError::RoutingLoop);
        }
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        _hdr: &HeaderSeq,
        _data: &[u8],
        _source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        // As a DirectEdge, we should never accept a raw message, as that must have
        // come from us.
        Err(InterfaceSendError::RoutingLoop)
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

/// Process one rx worker frame for direct edge workers
pub fn process_frame<N>(
    net_id: &mut Option<u16>,
    data: &[u8],
    nsh: &N,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
) where
    N: NetStackHandle,
{
    let Some(mut frame) = de_frame(data) else {
        warn!(
            "Decode error! Ignoring frame on net_id {}",
            net_id.unwrap_or(0)
        );
        return;
    };

    debug!("{}: Got Frame!", frame.hdr);

    let take_net = net_id.is_none()
        || net_id.is_some_and(|n| frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id);

    if take_net {
        nsh.stack().manage_profile(|im| {
            im.set_interface_state(
                ident.clone(),
                InterfaceState::Active {
                    net_id: frame.hdr.dst.network_id,
                    node_id: EDGE_NODE_ID,
                },
            )
            .unwrap();
        });
        *net_id = Some(frame.hdr.dst.network_id);
    }

    // If the message comes in and has a src net_id of zero,
    // we should rewrite it so it isn't later understood as a
    // local packet.
    //
    // TODO: accept any packet if we don't have a net_id yet?
    if let Some(net) = net_id.as_ref()
        && frame.hdr.src.network_id == 0
    {
        assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
        assert_ne!(frame.hdr.src.node_id, 2, "someone is pretending to be us?");

        frame.hdr.src.network_id = *net;
    }

    // TODO: if the destination IS self.net_id, we could rewrite the
    // dest net_id as zero to avoid a pass through the interface manager.
    //
    // If the dest is 0, should we rewrite the dest as self.net_id? This
    // is the opposite as above, but I dunno how that will work with responses
    let res = match frame.body {
        Ok(body) => nsh.stack().send_raw(&frame.hdr, body, ident),
        Err(e) => {
            // send_err requires a Header instead of a HeaderSeq, so we convert it
            let nshdr: Header = frame.hdr.clone().into();
            nsh.stack().send_err(&nshdr, e, Some(ident))
        }
    };

    match res {
        Ok(()) => {}
        Err(e) => {
            // TODO: match on error, potentially try to send NAK?
            warn!("send error: {:?}", e);
        }
    }
}
