//! Unified Router profile
//!
//! A single router profile that works on both `std` and `no_std` environments.
//! Manages up to `N` directly connected downstream (edge) devices, with up to
//! `S` additional seed-assigned routes for bridge devices.
//!
//! Uses [`heapless::Vec`] for storage, `EdgePort` for per-interface state,
//! and injectable [`RngCore`] for token generation.
//!
//! Requires either `std` or `nostd-seed-router` feature (for time and RNG).

// `web-time` re-exports `std::time` on native targets, and provides a
// `performance.now()`-based `Instant` on wasm32-unknown-unknown, where
// `std::time::Instant::now()` panics.
#[cfg(feature = "std")]
use web_time::{Duration, Instant};

#[cfg(all(not(feature = "std"), feature = "nostd-seed-router"))]
use embassy_time::{Duration, Instant};

use rand_core::RngCore;
use serde::Serialize;

use crate::{
    Header, HeaderSeq, ProtocolError,
    interface_manager::{
        AddressClaimError, AddressRefreshError, Interface, InterfaceSendError, InterfaceState,
        NodeClaimAssignment, Profile, SeedAssignmentError, SeedNetAssignment, SeedRefreshError,
        SetStateError,
        edge_port::{CENTRAL_NODE_ID, EDGE_NODE_ID, EdgePort},
    },
    logging::{debug, trace, warn},
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

/// Initial lease duration for a newly assigned seed net_id (seconds).
const INITIAL_SEED_ASSIGN_TIMEOUT: u16 = 30;
/// Maximum lease duration after refresh (seconds).
const MAX_SEED_ASSIGN_TIMEOUT: u16 = 120;
/// Refresh is allowed only when remaining time is less than this (seconds).
const MIN_SEED_REFRESH: u16 = 62;
/// Tombstone duration — how long a revoked net_id/node_id stays reserved,
/// measured from its lease expiration, before it can be reused (seconds).
const TOMBSTONE_DURATION_SECS: u64 = 30;

/// A directly connected downstream interface slot.
struct Slot<I: Interface> {
    ident: u8,
    port: EdgePort<I>,
    net_id: u16,
    #[cfg(feature = "std")]
    closer: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
}

/// A seed-assigned route for a bridge device's downstream.
struct SeedRoute {
    /// The assigned net_id.
    net_id: u16,
    /// The direct interface ident through which this route is reachable.
    via_ident: u8,
    /// Route state.
    kind: SeedRouteKind,
}

enum SeedRouteKind {
    /// Active lease.
    Active {
        source_net_id: u16,
        expiration: Instant,
        refresh_token: u64,
    },
    /// Tombstoned — net_id is reserved for a grace period to avoid stale routing.
    Tombstone { clear_time: Instant },
}

/// A claimed node_id on a bus-style interface.
struct NodeClaim {
    node_id: u8,
    /// The net_id of the interface this claim is on.
    source_net_id: u16,
    nonce: u64,
    kind: NodeClaimKind,
}

enum NodeClaimKind {
    Active {
        expiration: Instant,
        refresh_token: u64,
    },
    Tombstone {
        clear_time: Instant,
    },
}

/// Bitmap for O(1) node_id validation. Covers all 256 possible node_id values.
///
/// 32 bytes total. The router sets a bit on claim grant and clears it
/// when a tombstone expires.
pub struct NodeBitmap([u32; 8]);

impl NodeBitmap {
    const fn new() -> Self {
        Self([0; 8])
    }

    fn set(&mut self, node_id: u8) {
        self.0[node_id as usize / 32] |= 1 << (node_id as usize % 32);
    }

    fn clear(&mut self, node_id: u8) {
        self.0[node_id as usize / 32] &= !(1 << (node_id as usize % 32));
    }

    fn is_set(&self, node_id: u8) -> bool {
        self.0[node_id as usize / 32] & (1 << (node_id as usize % 32)) != 0
    }
}

/// The upstream interface port (bridge mode only).
struct UpstreamPort<I: Interface> {
    port: EdgePort<I>,
    #[cfg(feature = "std")]
    closer: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
}

/// Reserved ident for the upstream interface (bridge mode).
///
/// This is `u8::MAX` (255), which means a router can have at most 255
/// downstream interfaces (idents 0..254). The upstream, if present,
/// always uses this ident.
pub const UPSTREAM_IDENT: u8 = u8::MAX;

/// A router profile with seed router capability and optional upstream.
///
/// - `I`: Interface type (use [`multi_interface!`] for heterogeneous transports)
/// - `R`: RNG implementing [`RngCore`] for generating refresh tokens
/// - `N`: Maximum number of directly connected downstream interfaces
/// - `S`: Maximum number of seed-assigned routes (for bridge downstream networks)
/// - `C`: Maximum number of bus-style node_id claims (address claim protocol)
///
/// **Root mode** (`new`/`new_std`): no upstream, acts as a seed router.
/// **Bridge mode** (`new_bridge`): has an upstream interface, forwards
/// unroutable traffic upstream. The upstream discovers its net_id from
/// incoming frames (like a DirectEdge).
///
/// Works on both `std` and `no_std` (with `nostd-seed-router` feature).
///
/// [`multi_interface!`]: crate::multi_interface
pub struct Router<I: Interface, R: RngCore, const N: usize, const S: usize, const C: usize = 0> {
    slots: heapless::Vec<Slot<I>, N>,
    seed_routes: heapless::Vec<SeedRoute, S>,
    node_claims: heapless::Vec<NodeClaim, C>,
    claimed_bitmap: NodeBitmap,
    rng: R,
    upstream: Option<UpstreamPort<I>>,
}

/// Errors from [`Router::register_interface`].
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// All `N` slots are occupied.
    Full,
    /// No free net_id is available (every net_id in `1..u16::MAX` is in use).
    NetIdsExhausted,
}

/// Errors from [`Router::deregister_interface`].
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub enum DeregisterError {
    /// No interface with the given ident exists.
    NotFound,
}

impl<I: Interface, R: RngCore, const N: usize, const S: usize, const C: usize>
    Router<I, R, N, S, C>
{
    /// Create a new root router (no upstream) with the given RNG.
    pub fn new(rng: R) -> Self {
        let mut bitmap = NodeBitmap::new();
        // CENTRAL_NODE_ID is always valid (it's us)
        bitmap.set(CENTRAL_NODE_ID);
        // EDGE_NODE_ID is always valid for backwards compat with point-to-point
        bitmap.set(EDGE_NODE_ID);
        Self {
            slots: heapless::Vec::new(),
            seed_routes: heapless::Vec::new(),
            node_claims: heapless::Vec::new(),
            claimed_bitmap: bitmap,
            rng,
            upstream: None,
        }
    }

    /// Create a new bridge router with an upstream interface.
    ///
    /// The upstream interface starts in [`InterfaceState::Down`] and
    /// discovers its net_id from incoming frames. Use [`UPSTREAM_IDENT`]
    /// when creating the upstream RxWorker.
    pub fn new_bridge(rng: R, upstream_sink: I::Sink) -> Self {
        let mut bitmap = NodeBitmap::new();
        bitmap.set(CENTRAL_NODE_ID);
        bitmap.set(EDGE_NODE_ID);
        Self {
            slots: heapless::Vec::new(),
            seed_routes: heapless::Vec::new(),
            node_claims: heapless::Vec::new(),
            claimed_bitmap: bitmap,
            rng,
            upstream: Some(UpstreamPort {
                port: EdgePort::new_target(upstream_sink),
                #[cfg(feature = "std")]
                closer: None,
            }),
        }
    }

    /// Returns `true` if this router has an upstream interface (bridge mode).
    pub fn has_upstream(&self) -> bool {
        self.upstream.is_some()
    }

    /// Returns `true` if `net_id` is currently in use by a direct slot, a
    /// seed route (including tombstoned routes, whose net_id stays reserved
    /// for the grace period), or the upstream interface.
    fn net_id_in_use(&self, net_id: u16) -> bool {
        self.slots.iter().any(|s| s.net_id == net_id)
            || self.seed_routes.iter().any(|sr| sr.net_id == net_id)
            || self
                .upstream
                .as_ref()
                .is_some_and(|up| up.port.net_id() == Some(net_id))
    }

    /// Allocate the lowest free net_id in `1..u16::MAX`, reusing net_ids that
    /// have been freed (slot removed, or seed-route tombstone cleared).
    ///
    /// A tombstoned seed route still counts as in use, so its net_id is not
    /// handed out again until the grace period elapses and the route is
    /// removed by [`gc_seed_routes`](Self::gc_seed_routes). net_ids 0 and
    /// u16::MAX are reserved. Callers should run the relevant GC first so
    /// that cleared tombstones release their net_ids.
    fn alloc_net_id(&mut self) -> Result<u16, ()> {
        (1..u16::MAX).find(|&id| !self.net_id_in_use(id)).ok_or(())
    }

    /// Register a new downstream interface.
    ///
    /// Assigns a reusable ident and a unique net_id. The interface starts
    /// in [`InterfaceState::Active`] with [`CENTRAL_NODE_ID`] as the local
    /// node.
    ///
    /// Returns the assigned ident on success.
    pub fn register_interface(&mut self, sink: I::Sink) -> Result<u8, RegisterError> {
        // Reclaim net_ids from cleared seed-route tombstones before allocating.
        self.gc_seed_routes();
        if self.slots.is_full() {
            return Err(RegisterError::Full);
        }

        let ident = (0..N as u8)
            .find(|id| !self.slots.iter().any(|s| s.ident == *id))
            .expect("pigeonhole: fewer than N slots occupied, so a free ident in 0..N must exist");

        let net_id = self
            .alloc_net_id()
            .map_err(|()| RegisterError::NetIdsExhausted)?;

        let state = InterfaceState::Active {
            net_id,
            node_id: CENTRAL_NODE_ID,
        };

        self.slots
            .push(Slot {
                ident,
                port: EdgePort::new_controller(sink, state),
                net_id,
                #[cfg(feature = "std")]
                closer: None,
            })
            .ok()
            .expect("push after is_full check");

        Ok(ident)
    }

    /// Register a new downstream interface without assigning a net_id.
    ///
    /// The interface starts in [`InterfaceState::Down`] with `net_id = 0`.
    /// Use [`reassign_interface_net_id`](Profile::reassign_interface_net_id)
    /// (typically via [`bridge_seed_assign`](crate::net_stack::services::bridge_seed_assign))
    /// to assign a globally-routable net_id from a seed router.
    ///
    /// This is the preferred method for bridge downstream interfaces.
    pub fn register_interface_pending(&mut self, sink: I::Sink) -> Result<u8, RegisterError> {
        if self.slots.is_full() {
            return Err(RegisterError::Full);
        }

        let ident = (0..N as u8)
            .find(|id| !self.slots.iter().any(|s| s.ident == *id))
            .expect("pigeonhole: fewer than N slots occupied, so a free ident in 0..N must exist");

        self.slots
            .push(Slot {
                ident,
                port: EdgePort::new_controller(sink, InterfaceState::Down),
                net_id: 0,
                #[cfg(feature = "std")]
                closer: None,
            })
            .ok()
            .expect("push after is_full check");

        Ok(ident)
    }

    /// Remove a downstream interface by ident.
    ///
    /// Also tombstones any seed routes that were reachable through this interface.
    pub fn deregister_interface(&mut self, ident: u8) -> Result<(), DeregisterError> {
        let pos = self
            .slots
            .iter()
            .position(|s| s.ident == ident)
            .ok_or(DeregisterError::NotFound)?;

        let slot = self.slots.swap_remove(pos);

        // Signal workers to stop
        #[cfg(feature = "std")]
        if let Some(closer) = &slot.closer {
            closer.close();
        }

        let now = Instant::now();
        for sr in self.seed_routes.iter_mut() {
            if sr.via_ident == ident {
                sr.kind = SeedRouteKind::Tombstone {
                    clear_time: now + Duration::from_secs(TOMBSTONE_DURATION_SECS),
                };
            }
        }

        Ok(())
    }

    /// Get the net_id for a given ident, if it exists.
    pub fn net_id_of(&self, ident: u8) -> Option<u16> {
        self.slots
            .iter()
            .find(|s| s.ident == ident)
            .map(|s| s.net_id)
    }

    /// Store a closer WaitQueue for an interface, so that workers are
    /// notified when the interface is deregistered.
    #[cfg(feature = "std")]
    pub fn set_interface_closer(
        &mut self,
        ident: u8,
        closer: std::sync::Arc<maitake_sync::WaitQueue>,
    ) {
        if ident == UPSTREAM_IDENT {
            if let Some(up) = self.upstream.as_mut() {
                up.closer = Some(closer);
            }
        } else if let Some(slot) = self.slots.iter_mut().find(|s| s.ident == ident) {
            slot.closer = Some(closer);
        }
    }

    /// Return active net_ids.
    #[cfg(feature = "std")]
    pub fn get_nets(&self) -> Vec<u16> {
        self.slots
            .iter()
            .filter_map(|s| match s.port.state() {
                InterfaceState::Active { net_id, .. } => Some(net_id),
                _ => None,
            })
            .collect()
    }

    /// Garbage-collect expired claims: tombstone active expired claims,
    /// remove cleared tombstones and clear their bitmap bits.
    fn gc_node_claims(&mut self) {
        let now = Instant::now();
        // First pass: tombstone expired active claims
        for claim in self.node_claims.iter_mut() {
            if let NodeClaimKind::Active { expiration, .. } = &claim.kind
                && *expiration <= now
            {
                let clear_time = *expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                warn!("Node claim {} expired, tombstoning", claim.node_id);
                claim.kind = NodeClaimKind::Tombstone { clear_time };
            }
        }
        // Second pass: remove cleared tombstones, clear bitmap bits
        self.node_claims.retain(|c| match c.kind {
            NodeClaimKind::Active { .. } => true,
            NodeClaimKind::Tombstone { clear_time } => {
                if clear_time <= now {
                    // Only clear bitmap if no other active claim has this node_id
                    // (shouldn't happen normally, but be safe)
                    self.claimed_bitmap.clear(c.node_id);
                    false
                } else {
                    true
                }
            }
        });
    }

    /// Garbage-collect the seed route table: an expired active route becomes
    /// a tombstone (reserving its net_id for the grace period), and a
    /// tombstone whose grace period has elapsed is removed (freeing its
    /// net_id for reuse).
    fn gc_seed_routes(&mut self) {
        let now = Instant::now();
        self.seed_routes.retain_mut(|sr| match sr.kind {
            SeedRouteKind::Active { expiration, .. } => {
                if now >= expiration {
                    let clear_time = expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                    if now >= clear_time {
                        false
                    } else {
                        sr.kind = SeedRouteKind::Tombstone { clear_time };
                        true
                    }
                } else {
                    true
                }
            }
            SeedRouteKind::Tombstone { clear_time } => clear_time > now,
        });
    }

    /// Find the EdgePort to send through for a given destination net_id.
    ///
    /// Searches direct slots first, then seed routes.
    fn find(
        &mut self,
        hdr: &Header,
        source: Option<u8>,
    ) -> Result<&mut EdgePort<I>, InterfaceSendError> {
        if hdr.dst.port_id == 0 && hdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        // GC expired tombstones so they don't occupy slots indefinitely
        self.gc_seed_routes();

        // 1. Direct link lookup (skip pending slots with net_id=0 — they
        //    haven't been assigned a real net_id yet and must not intercept
        //    link-local frames destined for the upstream)
        if let Some(pos) = self
            .slots
            .iter()
            .position(|s| s.net_id != 0 && s.net_id == hdr.dst.network_id)
        {
            let slot = &self.slots[pos];
            if hdr.dst.node_id == CENTRAL_NODE_ID {
                return Err(InterfaceSendError::DestinationLocal);
            }
            if let Some(src_ident) = source
                && slot.ident == src_ident
            {
                return Err(InterfaceSendError::RoutingLoop);
            }
            return Ok(&mut self.slots[pos].port);
        }

        // 2. Seed route lookup
        let now = Instant::now();
        let sr = self
            .seed_routes
            .iter_mut()
            .find(|sr| sr.net_id == hdr.dst.network_id);

        let Some(sr) = sr else {
            // 3. Upstream fallback (bridge mode)
            return self.find_upstream(source);
        };

        match &mut sr.kind {
            SeedRouteKind::Active { expiration, .. } => {
                if *expiration <= now {
                    let clear_time = *expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                    warn!("Seed route net_id {} expired, tombstoning", sr.net_id);
                    sr.kind = SeedRouteKind::Tombstone { clear_time };
                    return Err(InterfaceSendError::NoRouteToDest);
                }
            }
            SeedRouteKind::Tombstone { .. } => {
                return Err(InterfaceSendError::NoRouteToDest);
            }
        }

        let via_ident = sr.via_ident;

        if let Some(src_ident) = source
            && via_ident == src_ident
        {
            return Err(InterfaceSendError::RoutingLoop);
        }

        let pos = self
            .slots
            .iter()
            .position(|s| s.ident == via_ident)
            .ok_or_else(|| {
                warn!(
                    "Seed route net_id {} has stale via_ident {}",
                    hdr.dst.network_id, via_ident
                );
                InterfaceSendError::NoRouteToDest
            })?;

        Ok(&mut self.slots[pos].port)
    }

    /// Try to route through the upstream interface (bridge mode only).
    fn find_upstream(
        &mut self,
        source: Option<u8>,
    ) -> Result<&mut EdgePort<I>, InterfaceSendError> {
        let Some(up) = self.upstream.as_mut() else {
            return Err(InterfaceSendError::NoRouteToDest);
        };
        // Don't route back to upstream if that's where it came from
        if source == Some(UPSTREAM_IDENT) {
            return Err(InterfaceSendError::RoutingLoop);
        }
        Ok(&mut up.port)
    }
}

// ---------------------------------------------------------------------------
// Profile implementation
// ---------------------------------------------------------------------------

impl<I: Interface, R: RngCore, const N: usize, const S: usize, const C: usize> Profile
    for Router<I, R, N, S, C>
{
    type InterfaceIdent = u8;

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }

            let mut any_good = false;
            for slot in self.slots.iter_mut() {
                if hdr.dst.network_id == slot.net_id {
                    continue;
                }
                let mut bhdr = hdr.clone();
                bhdr.dst.network_id = slot.net_id;
                bhdr.dst.node_id = EDGE_NODE_ID;
                any_good |= slot.port.send(&bhdr, data).is_ok();
            }
            // Also broadcast to upstream (bridge mode)
            if let Some(up) = self.upstream.as_mut() {
                any_good |= up.port.send(&hdr, data).is_ok();
            }
            if any_good {
                Ok(())
            } else {
                Err(InterfaceSendError::NoRouteToDest)
            }
        } else {
            let port = self.find(&hdr, None)?;
            port.send(&hdr, data)
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }
        let port = self.find(&hdr, source)?;
        port.send_err(&hdr, err)
    }

    fn send_raw(
        &mut self,
        hdr: &HeaderSeq,
        data: &[u8],
        source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        if hdr.decrement_ttl().is_err() {
            return Err(InterfaceSendError::NoRouteToDest);
        }

        if hdr.dst.port_id == 255 {
            if hdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
            let has_any_interface = !self.slots.is_empty() || self.upstream.is_some();
            if !has_any_interface {
                return Err(InterfaceSendError::NoRouteToDest);
            }

            let mut default_error = InterfaceSendError::RoutingLoop;
            let mut any_good = false;

            for slot in self.slots.iter_mut() {
                if source == slot.ident {
                    continue;
                }
                default_error = InterfaceSendError::NoRouteToDest;

                hdr.dst.network_id = slot.net_id;
                hdr.dst.node_id = EDGE_NODE_ID;
                any_good |= slot.port.send_raw(&hdr, data).is_ok();
            }
            // Also broadcast to upstream (bridge mode), unless source is upstream
            if let Some(up) = self.upstream.as_mut()
                && source != UPSTREAM_IDENT
            {
                default_error = InterfaceSendError::NoRouteToDest;
                any_good |= up.port.send_raw(&hdr, data).is_ok();
            }
            if any_good { Ok(()) } else { Err(default_error) }
        } else {
            let nshdr: Header = hdr.clone().into();
            let port = self.find(&nshdr, Some(source))?;
            port.send_raw(&hdr, data)
        }
    }

    fn interface_state(&mut self, ident: Self::InterfaceIdent) -> Option<InterfaceState> {
        if ident == UPSTREAM_IDENT {
            return self.upstream.as_ref().map(|up| up.port.state());
        }
        self.slots
            .iter()
            .find(|s| s.ident == ident)
            .map(|s| s.port.state())
    }

    fn set_interface_state(
        &mut self,
        ident: Self::InterfaceIdent,
        state: InterfaceState,
    ) -> Result<(), SetStateError> {
        if ident == UPSTREAM_IDENT {
            return self
                .upstream
                .as_mut()
                .ok_or(SetStateError::InterfaceNotFound)?
                .port
                .set_state(state);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|s| s.ident == ident)
            .ok_or(SetStateError::InterfaceNotFound)?;
        slot.port.set_state(state)
    }

    fn reassign_interface_net_id(
        &mut self,
        ident: Self::InterfaceIdent,
        new_net_id: u16,
    ) -> Result<(), SetStateError> {
        let slot = self
            .slots
            .iter_mut()
            .find(|s| s.ident == ident)
            .ok_or(SetStateError::InterfaceNotFound)?;
        slot.net_id = new_net_id;
        slot.port.set_state(InterfaceState::Active {
            net_id: new_net_id,
            node_id: CENTRAL_NODE_ID,
        })
    }

    fn request_seed_net_assign(
        &mut self,
        source_net: u16,
    ) -> Result<SeedNetAssignment, SeedAssignmentError> {
        self.gc_seed_routes();

        let via_ident = self
            .slots
            .iter()
            .find(|s| s.net_id == source_net)
            .map(|s| s.ident)
            .ok_or(SeedAssignmentError::UnknownSource)?;

        if self.seed_routes.is_full() {
            return Err(SeedAssignmentError::NetIdsExhausted);
        }

        let net_id = self
            .alloc_net_id()
            .map_err(|()| SeedAssignmentError::NetIdsExhausted)?;

        let refresh_token = self.rng.next_u64();
        let expiration = Instant::now() + Duration::from_secs(INITIAL_SEED_ASSIGN_TIMEOUT as u64);

        self.seed_routes
            .push(SeedRoute {
                net_id,
                via_ident,
                kind: SeedRouteKind::Active {
                    source_net_id: source_net,
                    expiration,
                    refresh_token,
                },
            })
            .ok()
            .expect("push after is_full check");

        Ok(SeedNetAssignment {
            net_id,
            expires_seconds: INITIAL_SEED_ASSIGN_TIMEOUT,
            max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
            min_refresh_seconds: MIN_SEED_REFRESH,
            refresh_token: refresh_token.to_le_bytes(),
        })
    }

    fn refresh_seed_net_assignment(
        &mut self,
        source_net: u16,
        refresh_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<SeedNetAssignment, SeedRefreshError> {
        let req_token = u64::from_le_bytes(refresh_token);
        // Pre-generate the new token before borrowing seed_routes
        let new_token = self.rng.next_u64();

        let sr = self
            .seed_routes
            .iter_mut()
            .find(|sr| sr.net_id == refresh_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;

        match &mut sr.kind {
            SeedRouteKind::Tombstone { .. } => Err(SeedRefreshError::AlreadyExpired),
            SeedRouteKind::Active {
                source_net_id,
                expiration,
                refresh_token: stored_token,
            } => {
                if *source_net_id != source_net || *stored_token != req_token {
                    return Err(SeedRefreshError::BadRequest);
                }

                let now = Instant::now();

                if *expiration <= now {
                    let clear_time = *expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                    warn!(
                        "Seed route net_id {} already expired during refresh",
                        refresh_net
                    );
                    sr.kind = SeedRouteKind::Tombstone { clear_time };
                    return Err(SeedRefreshError::AlreadyExpired);
                }

                let until_expired = *expiration - now;
                if until_expired > Duration::from_secs(MIN_SEED_REFRESH as u64) {
                    return Err(SeedRefreshError::TooSoon);
                }

                *expiration = now + Duration::from_secs(MAX_SEED_ASSIGN_TIMEOUT as u64);

                // Rotate the refresh token for replay protection
                *stored_token = new_token;

                Ok(SeedNetAssignment {
                    net_id: refresh_net,
                    expires_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    min_refresh_seconds: MIN_SEED_REFRESH,
                    refresh_token: new_token.to_le_bytes(),
                })
            }
        }
    }

    fn request_node_claim(
        &mut self,
        source_net: u16,
        candidate: u8,
        nonce: u64,
    ) -> Result<NodeClaimAssignment, AddressClaimError> {
        // GC expired claims first
        self.gc_node_claims();

        // Verify source net_id belongs to a known interface
        if !self.slots.iter().any(|s| s.net_id == source_net) {
            return Err(AddressClaimError::UnknownSource);
        }

        // Check if candidate is already in the table
        if let Some(existing) = self
            .node_claims
            .iter()
            .find(|c| c.node_id == candidate && c.source_net_id == source_net)
        {
            match &existing.kind {
                NodeClaimKind::Active {
                    expiration,
                    refresh_token,
                } => {
                    if existing.nonce == nonce {
                        // Duplicate request from the same device — return existing assignment
                        let remaining = (*expiration - Instant::now()).as_secs() as u16;
                        return Ok(NodeClaimAssignment {
                            node_id: candidate,
                            net_id: source_net,
                            expires_seconds: remaining,
                            max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                            min_refresh_seconds: MIN_SEED_REFRESH,
                            refresh_token: refresh_token.to_le_bytes(),
                        });
                    } else {
                        // Different nonce — conflict
                        return Err(AddressClaimError::Conflict);
                    }
                }
                NodeClaimKind::Tombstone { .. } => {
                    // Tombstoned — conflict (node_id reserved)
                    return Err(AddressClaimError::Conflict);
                }
            }
        }

        if self.node_claims.is_full() {
            return Err(AddressClaimError::Exhausted);
        }

        let refresh_token = self.rng.next_u64();
        let expiration =
            Instant::now() + Duration::from_secs(INITIAL_SEED_ASSIGN_TIMEOUT as u64);

        self.node_claims
            .push(NodeClaim {
                node_id: candidate,
                source_net_id: source_net,
                nonce,
                kind: NodeClaimKind::Active {
                    expiration,
                    refresh_token,
                },
            })
            .ok()
            .expect("push after is_full check");

        self.claimed_bitmap.set(candidate);

        Ok(NodeClaimAssignment {
            node_id: candidate,
            net_id: source_net,
            expires_seconds: INITIAL_SEED_ASSIGN_TIMEOUT,
            max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
            min_refresh_seconds: MIN_SEED_REFRESH,
            refresh_token: refresh_token.to_le_bytes(),
        })
    }

    fn refresh_node_claim(
        &mut self,
        source_net: u16,
        node_id: u8,
        refresh_token: [u8; 8],
    ) -> Result<NodeClaimAssignment, AddressRefreshError> {
        let req_token = u64::from_le_bytes(refresh_token);
        let new_token = self.rng.next_u64();

        let claim = self
            .node_claims
            .iter_mut()
            .find(|c| c.node_id == node_id && c.source_net_id == source_net)
            .ok_or(AddressRefreshError::UnknownNodeId)?;

        match &mut claim.kind {
            NodeClaimKind::Tombstone { .. } => Err(AddressRefreshError::AlreadyExpired),
            NodeClaimKind::Active {
                expiration,
                refresh_token: stored_token,
            } => {
                if *stored_token != req_token {
                    return Err(AddressRefreshError::BadRequest);
                }

                let now = Instant::now();

                if *expiration <= now {
                    let clear_time = *expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                    warn!("Node claim {} already expired during refresh", node_id);
                    claim.kind = NodeClaimKind::Tombstone { clear_time };
                    return Err(AddressRefreshError::AlreadyExpired);
                }

                let until_expired = *expiration - now;
                if until_expired > Duration::from_secs(MIN_SEED_REFRESH as u64) {
                    return Err(AddressRefreshError::TooSoon);
                }

                *expiration = now + Duration::from_secs(MAX_SEED_ASSIGN_TIMEOUT as u64);
                *stored_token = new_token;

                Ok(NodeClaimAssignment {
                    node_id,
                    net_id: source_net,
                    expires_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    max_refresh_seconds: MAX_SEED_ASSIGN_TIMEOUT,
                    min_refresh_seconds: MIN_SEED_REFRESH,
                    refresh_token: new_token.to_le_bytes(),
                })
            }
        }
    }

    fn is_node_claimed(&mut self, _net_id: u16, node_id: u8) -> bool {
        self.claimed_bitmap.is_set(node_id)
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors for std
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
impl<I: Interface, const N: usize, const S: usize, const C: usize>
    Router<I, rand::rngs::StdRng, N, S, C>
{
    /// Create a new root router using a randomly-seeded StdRng (Send + Sync).
    pub fn new_std() -> Self {
        use rand::SeedableRng;
        Self::new(rand::rngs::StdRng::from_os_rng())
    }

    /// Create a new bridge router with upstream, using a randomly-seeded StdRng.
    pub fn new_bridge_std(upstream_sink: I::Sink) -> Self {
        use rand::SeedableRng;
        Self::new_bridge(rand::rngs::StdRng::from_os_rng(), upstream_sink)
    }
}

#[cfg(feature = "std")]
impl<I: Interface, const N: usize, const S: usize, const C: usize> Default
    for Router<I, rand::rngs::StdRng, N, S, C>
{
    fn default() -> Self {
        Self::new_std()
    }
}

// ---------------------------------------------------------------------------
// FrameProcessor
// ---------------------------------------------------------------------------

/// Frame processor for the [`Router`] profile.
///
/// Uses a pre-assigned `net_id` and handles Inactive→Active transition
/// on the first successfully received frame.
pub struct RouterFrameProcessor {
    net_id: u16,
    activated: bool,
}

impl RouterFrameProcessor {
    /// Create a new processor with a pre-assigned net_id.
    pub fn new(net_id: u16) -> Self {
        Self {
            net_id,
            activated: false,
        }
    }
}

impl<N> crate::interface_manager::FrameProcessor<N> for RouterFrameProcessor
where
    N: crate::net_stack::NetStackHandle,
{
    fn process_frame(
        &mut self,
        data: &[u8],
        nsh: &N,
        ident: <<N as crate::net_stack::NetStackHandle>::Profile as crate::interface_manager::Profile>::InterfaceIdent,
    ) -> bool {
        // Sync net_id from the stack if still at the pending placeholder (0).
        // This handles the case where `reassign_interface_net_id` updated the
        // slot after this processor was created with `RouterFrameProcessor::new(0)`.
        if self.net_id == 0
            && let Some(InterfaceState::Active { net_id, .. }) = nsh
                .stack()
                .manage_profile(|im| im.interface_state(ident.clone()))
        {
            self.net_id = net_id;
        }

        process_frame(self.net_id, data, nsh, ident.clone());

        if !self.activated {
            let changed = nsh.stack().manage_profile(|im| {
                if matches!(
                    im.interface_state(ident.clone()),
                    Some(InterfaceState::Inactive)
                ) {
                    _ = im.set_interface_state(
                        ident,
                        InterfaceState::Active {
                            net_id: self.net_id,
                            node_id: CENTRAL_NODE_ID,
                        },
                    );
                    true
                } else {
                    false
                }
            });
            if changed {
                self.activated = true;
            }
            changed
        } else {
            false
        }
    }

    fn reset(&mut self) {
        self.activated = false;
    }
}

// ---------------------------------------------------------------------------
// Frame processing
// ---------------------------------------------------------------------------

/// Process one received frame for a Router RX worker.
pub fn process_frame<N>(
    net_id: u16,
    data: &[u8],
    nsh: &N,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
) where
    N: NetStackHandle,
{
    let Some(mut frame) = de_frame(data) else {
        warn!("Decode error! Ignoring frame on net_id {}", net_id);
        return;
    };

    trace!("{} got frame from {:?}", frame.hdr, ident);

    // Rewrite zero src net_id so it isn't mistaken for a local packet
    if frame.hdr.src.network_id == 0 {
        match frame.hdr.src.node_id {
            0 => {
                warn!(
                    "{}: device is sending us frames without a node id, ignoring",
                    frame.hdr
                );
                return;
            }
            CENTRAL_NODE_ID => {
                warn!("{}: device is sending us frames as us, ignoring", frame.hdr);
                return;
            }
            // Accept any non-zero, non-central node_id (bus-style support)
            _ => {}
        }

        frame.hdr.src.network_id = net_id;
    }

    // Bus address claim validation: check if the source node_id is claimed.
    // Skip for wildcard-port frames (port_id=0) which may be address claim requests
    // from nodes that don't have a claim yet.
    if frame.hdr.dst.port_id != 0 {
        let is_claimed = nsh
            .stack()
            .manage_profile(|im| im.is_node_claimed(net_id, frame.hdr.src.node_id));
        if !is_claimed {
            warn!(
                "{}: frame from unclaimed node_id {}, dropping",
                frame.hdr, frame.hdr.src.node_id
            );
            return;
        }
    }

    // Rewrite zero dst net_id to this interface's net_id (link-local addressing).
    // An edge that doesn't yet know its net_id sends dst=(0, CENTRAL_NODE_ID, port)
    // meaning "deliver to my directly connected router".
    if frame.hdr.dst.network_id == 0 {
        frame.hdr.dst.network_id = net_id;
    }

    let hdr = frame.hdr.clone();
    let nshdr: Header = hdr.clone().into();

    let res = match frame.body {
        Ok(body) => nsh.stack().send_raw(&hdr, body, ident.clone()),
        Err(e) => nsh.stack().send_err(&nshdr, e, Some(ident.clone())),
    };

    match res {
        Ok(()) => {
            debug!("{}: frame delivered", hdr);
        }
        Err(crate::net_stack::NetStackSendError::InterfaceSend(
            InterfaceSendError::PacketTooBig { mtu },
        )) => {
            warn!(
                "{} packet too big for outgoing interface (mtu={})",
                hdr, mtu
            );
            let err_hdr = Header {
                src: hdr.dst,
                dst: hdr.src,
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: crate::FrameKind::PROTOCOL_ERROR,
                ttl: crate::DEFAULT_TTL,
            };
            let _ = nsh.stack().send_err(
                &err_hdr,
                ProtocolError::IsePacketTooBig { mtu },
                Some(ident),
            );
        }
        Err(e) => {
            warn!("{} recv->send error: {:?}", hdr, e);
        }
    }
}
