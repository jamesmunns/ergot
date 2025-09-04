//! The Direct Router profile
//!
//! This is an early and simple router profile that can manage multiple directly connected
//! edge devices. It can route messages from one directly connected edge device to another,
//! as well as messages to/from itself and an edge device. It does not currently handle
//! multi-hop routing.

use log::{debug, trace, warn};

use crate::{
    Header, ProtocolError,
    interface_manager::{
        DeregisterError, Interface, InterfaceSendError, InterfaceState, Profile, SetStateError,
        profiles::direct_edge::CENTRAL_NODE_ID,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

use super::direct_edge::EDGE_NODE_ID;

pub mod tokio_tcp;

#[cfg(feature = "nusb-v0_1")]
pub mod nusb_0_1;

#[cfg(feature = "tokio-serial-v5")]
pub mod tokio_serial_5;

struct Node<I: Interface> {
    // TODO: can we JUST use an interface here, NOT a profile?
    edge: edge_target_hax::DirectEdge<I>,
    net_id: u16,
    ident: u64,
}

pub struct DirectRouter<I: Interface> {
    interface_ctr: u64,
    nodes: Vec<Node<I>>,
}

impl<I: Interface> Profile for DirectRouter<I> {
    type InterfaceIdent = u64;

    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &crate::Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
            let mut any_good = false;
            for p in self.nodes.iter_mut() {
                // Don't send back to the origin
                if hdr.dst.network_id == p.net_id {
                    continue;
                }
                let mut hdr = hdr.clone();
                // Make sure we still have ttl juice
                if hdr.decrement_ttl().is_err() {
                    continue;
                }
                hdr.dst.network_id = p.net_id;
                hdr.dst.node_id = EDGE_NODE_ID;
                any_good |= p.edge.send(&hdr, data).is_ok();
            }
            if any_good {
                Ok(())
            } else {
                Err(InterfaceSendError::NoRouteToDest)
            }
        } else {
            let intfc = self.find(hdr, None)?;
            intfc.send(hdr, data)
        }
    }

    fn send_err(
        &mut self,
        hdr: &crate::Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr, source)?;
        intfc.send_err(hdr, err)
    }

    fn send_raw(
        &mut self,
        hdr: &crate::Header,
        hdr_raw: &[u8],
        data: &[u8],
        source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
            let mut any_good = false;
            for p in self.nodes.iter_mut() {
                // Don't send back to the origin
                if hdr.dst.network_id == p.net_id {
                    continue;
                }
                // Don't send back to the origin
                // TODO: do we need both of these checks?
                if source == p.ident {
                    continue;
                }
                let mut hdr = hdr.clone();
                // Make sure we still have ttl juice
                if hdr.decrement_ttl().is_err() {
                    continue;
                }
                hdr.dst.network_id = p.net_id;
                hdr.dst.node_id = EDGE_NODE_ID;
                // TODO: this is wrong, hdr_raw and header could be out of sync!
                any_good |= p.edge.send_raw(&hdr, hdr_raw, data).is_ok();
            }
            if any_good {
                Ok(())
            } else {
                Err(InterfaceSendError::NoRouteToDest)
            }
        } else {
            let intfc = self.find(hdr, Some(source))?;
            intfc.send_raw(hdr, hdr_raw, data)
        }
    }

    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState> {
        let node = self.nodes.iter_mut().find(|n| n.ident == ident)?;
        node.edge.interface_state(())
    }

    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), SetStateError> {
        let Some(node) = self.nodes.iter_mut().find(|n| n.ident == ident) else {
            return Err(SetStateError::InterfaceNotFound);
        };
        node.edge.set_interface_state((), state)
    }
}

impl<I: Interface> DirectRouter<I> {
    pub fn new() -> Self {
        Self {
            interface_ctr: 0,
            nodes: vec![],
        }
    }

    pub fn get_nets(&mut self) -> Vec<u16> {
        self.nodes
            .iter_mut()
            .filter_map(|n| match n.edge.interface_state(())? {
                InterfaceState::Down => None,
                InterfaceState::Inactive => None,
                InterfaceState::ActiveLocal { .. } => None,
                InterfaceState::Active { net_id, node_id: _ } => Some(net_id),
            })
            .collect()
    }

    fn find<'b>(
        &'b mut self,
        ihdr: &Header,
        source: Option<<Self as Profile>::InterfaceIdent>,
    ) -> Result<&'b mut edge_target_hax::DirectEdge<I>, InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        if ihdr.dst.port_id == 0 && ihdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        let Ok(idx) = self
            .nodes
            .binary_search_by_key(&ihdr.dst.network_id, |n| n.net_id)
        else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        // If we're here, that means that the dst network_id matches the network_id
        // of `node[idx]`, but if the dst node_id is CENTRAL_NODE_ID: that means the
        // message is for US.
        if ihdr.dst.node_id == CENTRAL_NODE_ID {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // If the dest IS one of our interfaces, but NOT for us, and we received it,
        // then the only thing to do would be to send it back on the same interface
        // it came in on. That's a routing loop: don't do that!
        if let Some(src) = source
            && self.nodes[idx].ident == src
        {
            return Err(InterfaceSendError::RoutingLoop);
        }

        Ok(&mut self.nodes[idx].edge)
    }

    pub fn register_interface(&mut self, sink: I::Sink) -> Option<u64> {
        if self.nodes.is_empty() {
            let net_id = 1;
            let intfc_id = self.interface_ctr;
            self.interface_ctr += 1;

            self.nodes.push(Node {
                edge: edge_target_hax::DirectEdge::new_controller(
                    sink,
                    InterfaceState::Active {
                        net_id,
                        node_id: CENTRAL_NODE_ID,
                    },
                ),
                net_id,
                ident: intfc_id,
            });
            debug!("Alloc'd net_id 1");
            return Some(intfc_id);
        } else if self.nodes.len() >= 65534 {
            warn!("Out of netids!");
            return None;
        }

        let mut net_id = 1;
        // we're not empty, find the lowest free address by counting the
        // indexes, and if we find a discontinuity, allocate the first one.
        for intfc in self.nodes.iter() {
            if intfc.net_id > net_id {
                trace!("Found gap: {net_id}");
                break;
            }
            debug_assert!(intfc.net_id == net_id);
            net_id += 1;
        }
        // EITHER: We've found a gap that we can use, OR we've iterated all
        // interfaces, which means that we had contiguous allocations but we
        // have not exhausted the range.
        debug_assert!(net_id > 0 && net_id != u16::MAX);

        let intfc_id = self.interface_ctr;
        self.interface_ctr += 1;

        // todo we could probably just insert at (net_id - 1)?
        self.nodes.push(Node {
            edge: edge_target_hax::DirectEdge::new_controller(
                sink,
                InterfaceState::Active {
                    net_id,
                    node_id: CENTRAL_NODE_ID,
                },
            ),
            net_id,
            ident: intfc_id,
        });
        self.nodes.sort_unstable_by_key(|i| i.net_id);
        Some(intfc_id)
    }

    pub fn deregister_interface(&mut self, ident: u64) -> Result<(), DeregisterError> {
        let Some(pos) = self.nodes.iter().position(|n| n.ident == ident) else {
            return Err(DeregisterError::NoSuchInterface);
        };
        _ = self.nodes.remove(pos);
        Ok(())
    }
}

impl<I: Interface> Default for DirectRouter<I> {
    fn default() -> Self {
        Self::new()
    }
}

pub fn process_frame<N>(
    net_id: u16,
    data: &[u8],
    nsh: &N,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
) where
    N: NetStackHandle,
{
    // Successfully received a packet, now we need to
    // do something with it.
    if let Some(mut frame) = de_frame(data) {
        trace!("got frame: {:?} from {ident:?}", frame.hdr);
        // If the message comes in and has a src net_id of zero,
        // we should rewrite it so it isn't later understood as a
        // local packet.
        if frame.hdr.src.network_id == 0 {
            assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
            assert_ne!(frame.hdr.src.node_id, 1, "someone is pretending to be us?");

            frame.hdr.src.network_id = net_id;
        }
        // TODO: if the destination IS self.net_id, we could rewrite the
        // dest net_id as zero to avoid a pass through the interface manager.
        //
        // If the dest is 0, should we rewrite the dest as self.net_id? This
        // is the opposite as above, but I dunno how that will work with responses
        let hdr = frame.hdr.clone();
        let hdr: Header = hdr.into();

        let res = match frame.body {
            Ok(body) => nsh.stack().send_raw(&hdr, frame.hdr_raw, body, ident),
            Err(e) => nsh.stack().send_err(&hdr, e, Some(ident)),
        };
        match res {
            Ok(()) => {}
            Err(e) => {
                // TODO: match on error, potentially try to send NAK?
                warn!("recv->send error: {e:?}");
            }
        }
    } else {
        warn!("Decode error! Ignoring frame on net_id {}", net_id);
    }
}

mod edge_target_hax {
    use log::{debug, trace};
    use serde::Serialize;

    use crate::{
        Header, ProtocolError,
        interface_manager::{
            Interface, InterfaceSendError, InterfaceSink, InterfaceState, SetStateError,
            profiles::direct_edge::{CENTRAL_NODE_ID, EDGE_NODE_ID},
        },
        wire_frames::CommonHeader,
    };

    // TODO: call this something like "point to point edge"
    pub struct DirectEdge<I: Interface> {
        sink: I::Sink,
        seq_no: u16,
        state: InterfaceState,
        own_node_id: u8,
        other_node_id: u8,
    }

    impl<I: Interface> DirectEdge<I> {
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

    /// NOTE: this LOOKS like a profile impl, because it was, but it's actually not, because
    /// this version of DirectEdge only serves DirectRouter
    impl<I: Interface> DirectEdge<I> {
        pub(super) fn send<T: Serialize>(
            &mut self,
            hdr: &Header,
            data: &T,
        ) -> Result<(), InterfaceSendError> {
            let (intfc, header) = self.common_send(hdr)?;

            let res = intfc.send_ty(&header, hdr.any_all.as_ref(), data);

            match res {
                Ok(()) => Ok(()),
                Err(()) => Err(InterfaceSendError::InterfaceFull),
            }
        }

        pub(super) fn send_err(
            &mut self,
            hdr: &Header,
            err: ProtocolError,
        ) -> Result<(), InterfaceSendError> {
            let (intfc, header) = self.common_send(hdr)?;

            let res = intfc.send_err(&header, err);

            match res {
                Ok(()) => Ok(()),
                Err(()) => Err(InterfaceSendError::InterfaceFull),
            }
        }

        pub(super) fn send_raw(
            &mut self,
            hdr: &Header,
            hdr_raw: &[u8],
            data: &[u8],
        ) -> Result<(), InterfaceSendError> {
            let (intfc, header) = self.common_send(hdr)?;

            // TODO: this is wrong, hdr_raw and header could be out of sync if common_send
            // modified the header!
            let res = intfc.send_raw(&header, hdr_raw, data);

            match res {
                Ok(()) => Ok(()),
                Err(()) => Err(InterfaceSendError::InterfaceFull),
            }
        }

        pub(super) fn interface_state(&mut self, _ident: ()) -> Option<InterfaceState> {
            Some(self.state)
        }

        pub(super) fn set_interface_state(
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
}
