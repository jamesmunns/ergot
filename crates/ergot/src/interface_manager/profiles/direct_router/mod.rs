//! The Direct Router profile
//!
//! This is an early and simple router profile that can manage multiple directly connected
//! edge devices. It can route messages from one directly connected edge device to another,
//! as well as messages to/from itself and an edge device. It does not currently handle
//! multi-hop routing.

use std::collections::{BTreeMap, HashMap};

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
    edge: edge_interface_plus::EdgeInterfacePlus<I>,
    net_id: u16,
    ident: u64,
}

/// The "kind" of route, currently only "directly connected and assigned by us",
/// coming soon: remotely assigned net ids
enum RouteKind {
    DirectAssigned,
}

/// Route information
struct Route {
    /// The interface identifier for this route
    ident: u64,
    /// The kind of this route
    kind: RouteKind,
}

pub struct DirectRouter<I: Interface> {
    /// Monotonic interface counter
    interface_ctr: u64,
    /// Map of (Network ID => Route), where Route contains the interface ident
    routes: BTreeMap<u16, Route>,
    /// Map of (Interface Ident => Ident)
    direct_links: HashMap<u64, Node<I>>,
}

impl<I: Interface> Profile for DirectRouter<I> {
    type InterfaceIdent = u64;

    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &crate::Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        // Make sure we still have ttl juice
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }

            let mut any_good = false;
            for (_ident, p) in self.direct_links.iter_mut() {
                // Don't send back to the origin
                if hdr.dst.network_id == p.net_id {
                    continue;
                }
                let mut hdr = hdr.clone();
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
            let intfc = self.find(&hdr, None)?;
            intfc.send(&hdr, data)
        }
    }

    fn send_err(
        &mut self,
        hdr: &crate::Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        // Make sure we still have ttl juice
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        let intfc = self.find(&hdr, source)?;
        intfc.send_err(&hdr, err)
    }

    fn send_raw(
        &mut self,
        hdr: &crate::Header,
        hdr_raw: &[u8],
        data: &[u8],
        source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        // Make sure we still have ttl juice
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
            let mut any_good = false;
            for (_ident, p) in self.direct_links.iter_mut() {
                // Don't send back to the origin
                if source == p.ident {
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
            let intfc = self.find(&hdr, Some(source))?;
            intfc.send_raw(&hdr, hdr_raw, data)
        }
    }

    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState> {
        let node = self.direct_links.get_mut(&ident)?;
        node.edge.interface_state(())
    }

    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), SetStateError> {
        let Some(node) = self.direct_links.get_mut(&ident) else {
            return Err(SetStateError::InterfaceNotFound);
        };
        node.edge.set_interface_state((), state)
    }
}

impl<I: Interface> DirectRouter<I> {
    pub fn new() -> Self {
        Self {
            interface_ctr: 0,
            direct_links: HashMap::new(),
            routes: BTreeMap::new(),
        }
    }

    pub fn get_nets(&mut self) -> Vec<u16> {
        self.direct_links
            .iter_mut()
            .filter_map(|(_ident, n)| match n.edge.interface_state(())? {
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
    ) -> Result<&'b mut edge_interface_plus::EdgeInterfacePlus<I>, InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        if ihdr.dst.port_id == 0 && ihdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        // Find destination by net_id
        let Some(rte) = self.routes.get(&ihdr.dst.network_id) else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        // Cool, get the interface based on that ident
        let Some(intfc) = self.direct_links.get_mut(&rte.ident) else {
            // This is not cool. We have a route with no live direct link
            //  associated with it. Remove the route, return no route.
            warn!(
                "Stale route with net_id: {}, ident: {}, removing",
                ihdr.dst.network_id, rte.ident
            );
            self.routes.remove(&ihdr.dst.network_id);
            return Err(InterfaceSendError::NoRouteToDest);
        };

        // Is this actually for us?
        if (ihdr.dst.network_id == intfc.net_id) && (ihdr.dst.node_id == CENTRAL_NODE_ID) {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Is this NOT for us but the source and destination are the same?
        //
        // If the dest IS one of our interfaces, but NOT for us, and we received it,
        // then the only thing to do would be to send it back on the same interface
        // it came in on. That's a routing loop: don't do that!
        if let Some(src) = source
            && intfc.ident == src
        {
            return Err(InterfaceSendError::RoutingLoop);
        }

        Ok(&mut intfc.edge)
    }

    pub fn register_interface(&mut self, sink: I::Sink) -> Option<u64> {
        if self.direct_links.is_empty() {
            let net_id = 1;
            let intfc_id = self.interface_ctr;
            self.interface_ctr += 1;

            self.direct_links.insert(
                intfc_id,
                Node {
                    edge: edge_interface_plus::EdgeInterfacePlus::new_controller(
                        sink,
                        InterfaceState::Active {
                            net_id,
                            node_id: CENTRAL_NODE_ID,
                        },
                    ),
                    net_id,
                    ident: intfc_id,
                },
            );
            self.routes.insert(
                1,
                Route {
                    ident: intfc_id,
                    kind: RouteKind::DirectAssigned,
                },
            );
            debug!("Alloc'd net_id 1");
            return Some(intfc_id);
        } else if self.direct_links.len() >= 65534 {
            warn!("Out of netids!");
            return None;
        }

        let mut net_id = 1;
        // we're not empty, find the lowest free address by counting the
        // indexes, and if we find a discontinuity, allocate the first one.
        for (_ident, intfc) in self.direct_links.iter() {
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

        self.direct_links.insert(
            intfc_id,
            Node {
                edge: edge_interface_plus::EdgeInterfacePlus::new_controller(
                    sink,
                    InterfaceState::Active {
                        net_id,
                        node_id: CENTRAL_NODE_ID,
                    },
                ),
                net_id,
                ident: intfc_id,
            },
        );
        self.routes.insert(
            net_id,
            Route {
                ident: intfc_id,
                kind: RouteKind::DirectAssigned,
            },
        );
        Some(intfc_id)
    }

    pub fn deregister_interface(&mut self, ident: u64) -> Result<(), DeregisterError> {
        let Some(node) = self.direct_links.remove(&ident) else {
            return Err(DeregisterError::NoSuchInterface);
        };
        if let Some(rte) = self.routes.remove(&node.net_id) {
            debug!(
                "removing interface with net_id: {}, ident: {ident:?}",
                node.net_id
            );
            assert!(matches!(rte.kind, RouteKind::DirectAssigned));
        } else {
            unreachable!("Why doesn't this interface have a direct route?")
        }
        // Also remove any routes that rely on this interface
        self.routes.retain(|net_id, rte| {
            let keep = rte.ident != ident;
            if !keep {
                debug!("removing indirect route with net_id: {net_id}, ident: {ident:?}")
            }
            keep
        });

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

mod edge_interface_plus {
    use log::trace;
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
    pub struct EdgeInterfacePlus<I: Interface> {
        sink: I::Sink,
        seq_no: u16,
        state: InterfaceState,
        own_node_id: u8,
        other_node_id: u8,
    }

    impl<I: Interface> EdgeInterfacePlus<I> {
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

    impl<I: Interface> EdgeInterfacePlus<I> {
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

            // TODO: when this WAS a real Profile, we did a lot of these things, but
            // now they should be done by the router. For now, we just have asserts,
            // eventually we should relax this to debug_asserts?
            assert!(net_id != 0);
            let for_us = ihdr.dst.network_id == net_id && ihdr.dst.node_id == self.own_node_id;
            assert!(!for_us);

            let mut hdr = ihdr.clone();

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

            // If this message has no seq_no, assign it one
            let seq_no = hdr.seq_no.unwrap_or_else(|| {
                let seq_no = self.seq_no;
                self.seq_no = self.seq_no.wrapping_add(1);
                seq_no
            });

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
    impl<I: Interface> EdgeInterfacePlus<I> {
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
