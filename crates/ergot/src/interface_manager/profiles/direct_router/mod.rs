//! The Direct Router profile
//!
//! This is an early and simple router profile that can manage multiple directly connected
//! edge devices. It can route messages from one directly connected edge device to another,
//! as well as messages to/from itself and an edge device. It does not currently handle
//! multi-hop routing.

#[cfg(all(feature = "embassy-net-v0_7", not(feature = "_all-features-hack")))]
use embassy_time::Duration;
#[cfg(all(feature = "embassy-net-v0_7", not(feature = "_all-features-hack")))]
use embassy_time::Instant;
#[cfg(any(feature = "std", feature = "_all-features-hack"))]
use std::time::{Duration, Instant};

#[cfg(feature = "std")]
use std::collections::{BTreeMap, HashMap};

use crate::logging::{debug, info, trace, warn};
#[cfg(feature = "std")]
use rand::Rng;

use crate::{
    Header, ProtocolError,
    interface_manager::{
        DeregisterError, Interface, InterfaceSendError, InterfaceState, Profile,
        SeedAssignmentError, SeedNetAssignment, SeedRefreshError, SetStateError,
        profiles::direct_edge::CENTRAL_NODE_ID,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

use super::direct_edge::EDGE_NODE_ID;

#[cfg(feature = "tokio-std")]
pub mod tokio_tcp;
#[cfg(feature = "tokio-std")]
pub mod tokio_udp;

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
#[derive(Clone)]
enum RouteKind {
    /// This route is associated with a node we are DIRECTLY connected to
    DirectAssigned,
    /// This route was assigned by us as a seed router
    SeedAssigned {
        source_net_id: u16,
        expiration_time: Instant,
        refresh_token: u64,
    },
    /// This route is inactive
    Tombstone { clear_time: Instant },
}

/// Route information
#[derive(Clone)]
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

/// The timeout duration in seconds for an initial net_id assignment as a seed router
const INITIAL_SEED_ASSIGN_TIMEOUT: u16 = 30;
/// The max timeout duration in seconds for a net_id assignment
const MAX_SEED_ASSIGN_TIMEOUT: u16 = 120;
/// There must be LESS than this many seconds left until expiration to allow for a refresh
const MIN_SEED_REFRESH: u16 = 62;

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
        hdr: &crate::HeaderSeq,
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
            if self.direct_links.is_empty() {
                return Err(InterfaceSendError::NoRouteToDest);
            }

            // use this error until we find a non-origin destination
            let mut default_error = InterfaceSendError::RoutingLoop;

            let mut any_good = false;
            for (_ident, p) in self.direct_links.iter_mut() {
                // Don't send back to the origin
                if source == p.ident {
                    continue;
                }
                // if there's a non-origin destination, and we can't send to it, then use this error
                default_error = InterfaceSendError::NoRouteToDest;

                // For broadcast messages, rewrite the destination address
                // to the address of the next hop.
                hdr.dst.network_id = p.net_id;
                hdr.dst.node_id = EDGE_NODE_ID;
                any_good |= p.edge.send_raw(&hdr, data).is_ok();
            }
            if any_good { Ok(()) } else { Err(default_error) }
        } else {
            let nshdr = hdr.clone().into();
            let intfc = self.find(&nshdr, Some(source))?;
            intfc.send_raw(&hdr, data)
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

    fn request_seed_net_assign(
        &mut self,
        source_net: u16,
    ) -> Result<SeedNetAssignment, SeedAssignmentError> {
        // Get the route for the source net
        let Some(rte) = self.routes.get(&source_net) else {
            return Err(SeedAssignmentError::UnknownSource);
        };
        let rte = rte.clone();
        // Get a new net id
        let Some(new_net_id) = self.find_free_net_id() else {
            return Err(SeedAssignmentError::NetIdsExhausted);
        };
        // Pick a random refresh token
        let refresh_token = rand::rng().random();

        // Insert this route, initially with a low refresh time, allowing the
        // remote device to immediately refresh, acting as an "acknowledgement"
        // of the assignment
        self.routes.insert(
            new_net_id,
            Route {
                ident: rte.ident,
                kind: RouteKind::SeedAssigned {
                    source_net_id: source_net,
                    expiration_time: Instant::now()
                        + Duration::from_secs(INITIAL_SEED_ASSIGN_TIMEOUT.into()),
                    refresh_token,
                },
            },
        );

        Ok(SeedNetAssignment {
            net_id: new_net_id,
            expires_seconds: INITIAL_SEED_ASSIGN_TIMEOUT,
            max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
            min_refresh_seconds: MIN_SEED_REFRESH,
            refresh_token: refresh_token.to_le_bytes(),
        })
    }

    fn refresh_seed_net_assignment(
        &mut self,
        req_source_net: u16,
        req_refresh_net: u16,
        req_refresh_token: [u8; 8],
    ) -> Result<SeedNetAssignment, SeedRefreshError> {
        let Some(rte) = self.routes.get_mut(&req_refresh_net) else {
            return Err(SeedRefreshError::UnknownNetId);
        };
        let req_refresh_token_u64 = u64::from_le_bytes(req_refresh_token);
        match &mut rte.kind {
            RouteKind::DirectAssigned => Err(SeedRefreshError::NotAssigned),
            RouteKind::Tombstone { clear_time: _ } => Err(SeedRefreshError::AlreadyExpired),
            RouteKind::SeedAssigned {
                source_net_id,
                expiration_time,
                refresh_token,
            } => {
                let bad_net = *source_net_id != req_source_net;
                let bad_tok = *refresh_token != req_refresh_token_u64;
                if bad_net || bad_tok {
                    return Err(SeedRefreshError::BadRequest);
                }
                let now = Instant::now();
                // Are we ALREADY expired?
                if *expiration_time <= now {
                    warn!("Tombstoning net_id: {}", req_refresh_net);
                    rte.kind = RouteKind::Tombstone {
                        clear_time: now + Duration::from_secs(30),
                    };
                    return Err(SeedRefreshError::AlreadyExpired);
                }
                // Are we TOO SOON for a refresh?
                // Note: we already checked if the expiration time is in the past
                let until_expired = *expiration_time - now;
                if until_expired > Duration::from_secs(MIN_SEED_REFRESH.into()) {
                    return Err(SeedRefreshError::TooSoon);
                }

                // Looks good: update the expiration time
                *expiration_time = now + Duration::from_secs(MAX_SEED_ASSIGN_TIMEOUT.into());
                Ok(SeedNetAssignment {
                    net_id: req_refresh_net,
                    expires_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    min_refresh_seconds: MIN_SEED_REFRESH,
                    refresh_token: req_refresh_token,
                })
            }
        }
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
        hdr: &Header,
        source: Option<<Self as Profile>::InterfaceIdent>,
    ) -> Result<&'b mut edge_interface_plus::EdgeInterfacePlus<I>, InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        if hdr.dst.port_id == 0 && hdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        // Find destination by net_id
        let Some(rte) = self.routes.get_mut(&hdr.dst.network_id) else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        // Do an expiration check for the given route
        match rte.kind {
            RouteKind::DirectAssigned => {}
            RouteKind::SeedAssigned {
                expiration_time, ..
            } => {
                let now = Instant::now();
                if expiration_time <= now {
                    warn!("Tombstoning net_id: {}", hdr.dst.network_id);
                    rte.kind = RouteKind::Tombstone {
                        clear_time: now + Duration::from_secs(30),
                    };
                    return Err(InterfaceSendError::NoRouteToDest);
                }
            }
            RouteKind::Tombstone { clear_time } => {
                let now = Instant::now();
                if clear_time <= now {
                    // times up, get gone.
                    self.routes.remove(&hdr.dst.network_id);
                }
                return Err(InterfaceSendError::NoRouteToDest);
            }
        }

        // Cool, get the interface based on that ident
        let Some(intfc) = self.direct_links.get_mut(&rte.ident) else {
            // This is not cool. We have a route with no live direct link
            //  associated with it. Remove the route, return no route.
            warn!(
                "Stale route with net_id: {}, ident: {}, removing",
                hdr.dst.network_id, rte.ident
            );
            self.routes.remove(&hdr.dst.network_id);
            return Err(InterfaceSendError::NoRouteToDest);
        };

        // Is this actually for us?
        if (hdr.dst.network_id == intfc.net_id) && (hdr.dst.node_id == CENTRAL_NODE_ID) {
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

    fn find_free_net_id(&mut self) -> Option<u16> {
        if self.routes.is_empty() {
            assert!(self.direct_links.is_empty());
            Some(1)
        } else if self.routes.len() == 65534 {
            warn!("Out of netids!");
            None
        } else {
            let mut new_net_id = 1;

            let mut to_evict = None;
            let now = Instant::now();
            for (net_id, rte) in self.routes.iter_mut() {
                match rte.kind {
                    RouteKind::DirectAssigned => {
                        // We don't need to care about timeouts for directly assigned routes
                    }
                    RouteKind::SeedAssigned {
                        expiration_time, ..
                    } => {
                        // If a route has expired, mark it tombstoned to avoid re-using it for a bit
                        if expiration_time <= now {
                            warn!("Tombstoning net_id: {}", net_id);
                            rte.kind = RouteKind::Tombstone {
                                clear_time: now + Duration::from_secs(30),
                            };
                        }
                    }
                    RouteKind::Tombstone { clear_time } => {
                        // If we've cleared the tombstone time, then re-use this net id
                        if clear_time <= now {
                            info!("Reclaiming tombstoned net_id: {}", net_id);
                            to_evict = Some(*net_id);
                            break;
                        }
                    }
                }
                if *net_id > new_net_id {
                    trace!("Found gap: {}", net_id);
                    break;
                }
                debug_assert!(*net_id == new_net_id);
                new_net_id += 1;
            }
            if let Some(evicted) = to_evict {
                self.routes.remove(&evicted);
                Some(evicted)
            } else {
                // EITHER: We've found a gap that we can use, OR we've iterated all
                // interfaces, which means that we had contiguous allocations but we
                // have not exhausted the range.
                debug_assert!(new_net_id > 0 && new_net_id != u16::MAX);
                Some(new_net_id)
            }
        }
    }

    pub fn register_interface(&mut self, sink: I::Sink) -> Option<u64> {
        let net_id = self.find_free_net_id()?;

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
                "removing interface with net_id: {}, ident: {:?}",
                node.net_id, ident,
            );
            assert!(matches!(rte.kind, RouteKind::DirectAssigned));
        } else {
            unreachable!("Why doesn't this interface have a direct route?")
        }
        // Also remove any routes that rely on this interface
        self.routes.retain(|net_id, rte| {
            let keep = rte.ident != ident;
            if !keep {
                debug!(
                    "removing indirect route with net_id: {}, ident: {:?}",
                    net_id, ident
                )
            }
            keep
        });

        // re-insert route as a tombstone
        self.routes.insert(
            node.net_id,
            Route {
                ident,
                kind: RouteKind::Tombstone {
                    clear_time: Instant::now() + Duration::from_secs(30),
                },
            },
        );

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
        trace!("{} got frame from {:?}", frame.hdr, ident);
        // If the message comes in and has a src net_id of zero,
        // we should rewrite it so it isn't later understood as a
        // local packet.
        if frame.hdr.src.network_id == 0 {
            match frame.hdr.src.node_id {
                0 => {
                    log::warn!(
                        "{}: device is sending us frames without a node id, ignoring",
                        frame.hdr
                    );
                    return;
                }
                CENTRAL_NODE_ID => {
                    log::warn!("{}: device is sending us frames as us, ignoring", frame.hdr);
                    return;
                }
                EDGE_NODE_ID => {}
                _ => {
                    log::warn!(
                        "{}: device is sending us frames with a bad node id, ignoring",
                        frame.hdr
                    );
                    return;
                }
            }

            frame.hdr.src.network_id = net_id;
        }
        // TODO: if the destination IS self.net_id, we could rewrite the
        // dest net_id as zero to avoid a pass through the interface manager.
        //
        // If the dest is 0, should we rewrite the dest as self.net_id? This
        // is the opposite as above, but I dunno how that will work with responses
        let hdr = frame.hdr.clone();
        let nshdr: Header = hdr.clone().into();

        let res = match frame.body {
            Ok(body) => nsh.stack().send_raw(&hdr, body, ident),
            Err(e) => nsh.stack().send_err(&nshdr, e, Some(ident)),
        };
        match res {
            Ok(()) => {}
            Err(e) => {
                // TODO: match on error, potentially try to send NAK?
                warn!("{} recv->send error: {:?}", frame.hdr, e);
            }
        }
    } else {
        warn!("Decode error! Ignoring frame on net_id {}", net_id);
    }
}

mod edge_interface_plus {
    use crate::logging::trace;
    use serde::Serialize;

    use crate::{
        Header, HeaderSeq, ProtocolError,
        interface_manager::{
            Interface, InterfaceSendError, InterfaceSink, InterfaceState, SetStateError,
            profiles::direct_edge::{CENTRAL_NODE_ID, EDGE_NODE_ID},
        },
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
            hdr: &Header,
        ) -> Result<(&'b mut I::Sink, HeaderSeq), InterfaceSendError> {
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

            trace!("{} common_send", hdr);

            // TODO: when this WAS a real Profile, we did a lot of these things, but
            // now they should be done by the router. For now, we just have asserts,
            // eventually we should relax this to debug_asserts?
            assert!(net_id != 0);
            let for_us = hdr.dst.network_id == net_id && hdr.dst.node_id == self.own_node_id;
            assert!(!for_us);

            let mut hdr = hdr.clone();

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

    /// NOTE: this LOOKS like a profile impl, because it was, but it's actually not, because
    /// this version of DirectEdge only serves DirectRouter
    impl<I: Interface> EdgeInterfacePlus<I> {
        pub(super) fn send<T: Serialize>(
            &mut self,
            hdr: &Header,
            data: &T,
        ) -> Result<(), InterfaceSendError> {
            let (intfc, header) = self.common_send(hdr)?;

            let res = intfc.send_ty(&header, data);

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
            hdr: &HeaderSeq,
            data: &[u8],
        ) -> Result<(), InterfaceSendError> {
            let nshdr: Header = hdr.clone().into();
            let (intfc, header) = self.common_send(&nshdr)?;
            let res = intfc.send_raw(&header, data);

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
