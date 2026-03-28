//! Edge Port
//!
//! An [`EdgePort`] is a single point-to-point interface state machine.
//! It manages the sink, sequence numbers, interface state, and address
//! rewriting for one side of a point-to-point link.
//!
//! [`EdgePort`] is the shared building block used by both [`DirectEdge`]
//! (single-interface edge devices) and [`DirectRouter`] (multi-interface
//! routing devices). Previously this logic existed as separate copies in
//! each profile; this module unifies them.
//!
//! [`EdgePort`] does NOT call [`Header::decrement_ttl`] — that is the
//! responsibility of the calling [`Profile`], which may need to decrement
//! TTL once for the entire routing decision rather than per-port.
//!
//! [`DirectEdge`]: crate::interface_manager::profiles::direct_edge::DirectEdge
//! [`DirectRouter`]: crate::interface_manager::profiles::router::Router
//! [`Profile`]: crate::interface_manager::Profile

use serde::Serialize;

use crate::{
    Header, HeaderSeq, ProtocolError,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceSink, InterfaceState, SetStateError,
    },
    logging::trace,
};

/// Node ID for the central (controller/router) side of a point-to-point link.
pub const CENTRAL_NODE_ID: u8 = 1;
/// Node ID for the edge (target/downstream) side of a point-to-point link.
pub const EDGE_NODE_ID: u8 = 2;

/// A single point-to-point interface port.
///
/// Manages the outgoing sink, per-port sequence numbers, interface state,
/// and header rewriting (source address, broadcast destination, seq_no
/// assignment).
pub struct EdgePort<I: Interface> {
    sink: I::Sink,
    seq_no: u16,
    state: InterfaceState,
    own_node_id: u8,
    other_node_id: u8,
}

impl<I: Interface> EdgePort<I> {
    /// Create a new port in the "target" (edge) role.
    ///
    /// The local side uses [`EDGE_NODE_ID`] (2), the remote side is
    /// [`CENTRAL_NODE_ID`] (1). State starts as [`InterfaceState::Down`].
    pub const fn new_target(sink: I::Sink) -> Self {
        Self {
            sink,
            seq_no: 0,
            state: InterfaceState::Down,
            own_node_id: EDGE_NODE_ID,
            other_node_id: CENTRAL_NODE_ID,
        }
    }

    /// Create a new port in the "controller" (central/router) role.
    ///
    /// The local side uses [`CENTRAL_NODE_ID`] (1), the remote side is
    /// [`EDGE_NODE_ID`] (2).
    pub const fn new_controller(sink: I::Sink, state: InterfaceState) -> Self {
        Self {
            sink,
            seq_no: 0,
            state,
            own_node_id: CENTRAL_NODE_ID,
            other_node_id: EDGE_NODE_ID,
        }
    }

    /// Returns the current [`InterfaceState`] of this port.
    pub fn state(&self) -> InterfaceState {
        self.state
    }

    /// Returns the net_id if the interface is [`InterfaceState::Active`],
    /// or `None` otherwise.
    #[allow(dead_code)]
    pub fn net_id(&self) -> Option<u16> {
        match self.state {
            InterfaceState::Active { net_id, .. } => Some(net_id),
            _ => None,
        }
    }

    /// Update the [`InterfaceState`] of this port.
    ///
    /// Returns [`SetStateError::InvalidNodeId`] if the state contains a
    /// `node_id` that does not match this port's role.
    pub fn set_state(&mut self, state: InterfaceState) -> Result<(), SetStateError> {
        match state {
            InterfaceState::Down | InterfaceState::Inactive => {
                self.state = state;
            }
            InterfaceState::ActiveLocal { node_id } => {
                if node_id != self.own_node_id {
                    return Err(SetStateError::InvalidNodeId);
                }
                self.state = state;
            }
            InterfaceState::Active { node_id, .. } => {
                if node_id != self.own_node_id {
                    return Err(SetStateError::InvalidNodeId);
                }
                self.state = state;
            }
        }
        Ok(())
    }

    /// Prepare to send a message through this port.
    ///
    /// Performs state checks, source address rewriting, broadcast destination
    /// rewriting, and sequence number assignment. Returns a mutable reference
    /// to the sink and the finalized header.
    ///
    /// The caller is responsible for decrementing TTL before calling this
    /// method.
    fn common_send<'b>(
        &'b mut self,
        hdr: &Header,
    ) -> Result<(&'b mut I::Sink, HeaderSeq), InterfaceSendError> {
        let net_id = match self.state {
            InterfaceState::Active { net_id, .. } => net_id,
            _ => return Err(InterfaceSendError::NoRouteToDest),
        };

        if net_id == 0 {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        // If the packet is destined for us, signal back to the caller
        if hdr.dst.network_id == net_id && hdr.dst.node_id == self.own_node_id {
            return Err(InterfaceSendError::DestinationLocal);
        }

        trace!("{}: EdgePort::common_send", hdr);

        let mut hdr = hdr.clone();

        // Rewrite local source address with this interface's identity
        if hdr.src.net_node_any() {
            hdr.src.network_id = net_id;
            hdr.src.node_id = self.own_node_id;
        }

        // Rewrite broadcast destination to the remote node
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = net_id;
            hdr.dst.node_id = self.other_node_id;
        }

        // Assign a sequence number if the message doesn't have one
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

    /// Send a serializable message through this port.
    ///
    /// The caller must decrement TTL before calling.
    pub fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let (sink, header) = self.common_send(hdr)?;
        sink.send_ty(&header, data)
            .map_err(|()| InterfaceSendError::InterfaceFull)
    }

    /// Send a protocol error through this port.
    ///
    /// The caller must decrement TTL before calling.
    pub fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError> {
        let (sink, header) = self.common_send(hdr)?;
        sink.send_err(&header, err)
            .map_err(|()| InterfaceSendError::InterfaceFull)
    }

    /// Send a pre-serialized (raw) message through this port.
    ///
    /// The caller must decrement TTL before calling. The `hdr` is a
    /// [`HeaderSeq`] because raw messages have already been assigned a
    /// sequence number by the originator; however, `common_send` may
    /// reassign one.
    #[allow(dead_code)]
    pub fn send_raw(&mut self, hdr: &HeaderSeq, data: &[u8]) -> Result<(), InterfaceSendError> {
        let nshdr: Header = hdr.clone().into();
        let (sink, header) = self.common_send(&nshdr)?;
        sink.send_raw(&header, data)
            .map_err(|()| InterfaceSendError::InterfaceFull)
    }
}
