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
        profiles::direct_edge::{CENTRAL_NODE_ID, DirectEdge},
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
    edge: DirectEdge<I>,
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
            let intfc = self.find(hdr)?;
            intfc.send(hdr, data)
        }
    }

    fn send_err(
        &mut self,
        hdr: &crate::Header,
        err: ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send_err(hdr, err)
    }

    fn send_raw(
        &mut self,
        hdr: &crate::Header,
        hdr_raw: &[u8],
        data: &[u8],
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
                // TODO: this is wrong, hdr_raw and header could be out of sync!
                any_good |= p.edge.send_raw(&hdr, hdr_raw, data).is_ok();
            }
            if any_good {
                Ok(())
            } else {
                Err(InterfaceSendError::NoRouteToDest)
            }
        } else {
            let intfc = self.find(hdr)?;
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

    fn find<'b>(&'b mut self, ihdr: &Header) -> Result<&'b mut DirectEdge<I>, InterfaceSendError> {
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

        Ok(&mut self.nodes[idx].edge)
    }

    pub fn register_interface(&mut self, sink: I::Sink) -> Option<u64> {
        if self.nodes.is_empty() {
            let net_id = 1;
            let intfc_id = self.interface_ctr;
            self.interface_ctr += 1;

            self.nodes.push(Node {
                edge: DirectEdge::new_controller(
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
            edge: DirectEdge::new_controller(
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

pub fn process_frame<N>(net_id: u16, data: &[u8], nsh: &N)
where
    N: NetStackHandle,
{
    // Successfully received a packet, now we need to
    // do something with it.
    if let Some(mut frame) = de_frame(data) {
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
            Ok(body) => nsh.stack().send_raw(&hdr, frame.hdr_raw, body),
            Err(e) => nsh.stack().send_err(&hdr, e),
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
