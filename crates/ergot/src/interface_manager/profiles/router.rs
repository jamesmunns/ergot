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

#[cfg(feature = "std")]
use std::time::{Duration, Instant};

#[cfg(all(not(feature = "std"), feature = "nostd-seed-router"))]
use embassy_time::{Duration, Instant};

use rand_core::RngCore;
use serde::Serialize;

use crate::{
    Header, HeaderSeq, ProtocolError,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceState, Profile, SeedAssignmentError,
        SeedNetAssignment, SeedRefreshError, SetStateError,
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
/// Tombstone duration — how long a revoked net_id is kept before reuse (seconds).
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
///
/// **Root mode** (`new`/`new_std`): no upstream, acts as a seed router.
/// **Bridge mode** (`new_bridge`): has an upstream interface, forwards
/// unroutable traffic upstream. The upstream discovers its net_id from
/// incoming frames (like a DirectEdge).
///
/// Works on both `std` and `no_std` (with `nostd-seed-router` feature).
///
/// [`multi_interface!`]: crate::multi_interface
pub struct Router<I: Interface, R: RngCore, const N: usize, const S: usize> {
    net_id_ctr: u16,
    slots: heapless::Vec<Slot<I>, N>,
    seed_routes: heapless::Vec<SeedRoute, S>,
    rng: R,
    upstream: Option<UpstreamPort<I>>,
}

/// Errors from [`Router::register_interface`].
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// All `N` slots are occupied.
    Full,
    /// The monotonic net_id counter has been exhausted.
    NetIdsExhausted,
}

/// Errors from [`Router::deregister_interface`].
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, PartialEq, Eq)]
pub enum DeregisterError {
    /// No interface with the given ident exists.
    NotFound,
}

impl<I: Interface, R: RngCore, const N: usize, const S: usize> Router<I, R, N, S> {
    /// Create a new root router (no upstream) with the given RNG.
    pub fn new(rng: R) -> Self {
        Self {
            net_id_ctr: 1,
            slots: heapless::Vec::new(),
            seed_routes: heapless::Vec::new(),
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
        Self {
            net_id_ctr: 1,
            slots: heapless::Vec::new(),
            seed_routes: heapless::Vec::new(),
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

    /// Allocate the next unique net_id (monotonic, never reused).
    fn alloc_net_id(&mut self) -> Result<u16, ()> {
        let net_id = self.net_id_ctr;
        if net_id == 0 || net_id == u16::MAX {
            return Err(());
        }
        self.net_id_ctr = self.net_id_ctr.checked_add(1).ok_or(())?;
        Ok(net_id)
    }

    /// Register a new downstream interface.
    ///
    /// Assigns a reusable ident and a unique net_id. The interface starts
    /// in [`InterfaceState::Active`] with [`CENTRAL_NODE_ID`] as the local
    /// node.
    ///
    /// Returns the assigned ident on success.
    pub fn register_interface(&mut self, sink: I::Sink) -> Result<u8, RegisterError> {
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

    /// Garbage-collect expired tombstones from the seed route table.
    fn gc_seed_routes(&mut self) {
        let now = Instant::now();
        self.seed_routes.retain(|sr| match sr.kind {
            SeedRouteKind::Active { .. } => true,
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

        // 1. Direct link lookup
        if let Some(pos) = self
            .slots
            .iter()
            .position(|s| s.net_id == hdr.dst.network_id)
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
                    warn!("Seed route net_id {} expired, tombstoning", sr.net_id);
                    sr.kind = SeedRouteKind::Tombstone {
                        clear_time: now + Duration::from_secs(TOMBSTONE_DURATION_SECS),
                    };
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

impl<I: Interface, R: RngCore, const N: usize, const S: usize> Profile for Router<I, R, N, S> {
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
                    warn!(
                        "Seed route net_id {} already expired during refresh",
                        refresh_net
                    );
                    sr.kind = SeedRouteKind::Tombstone {
                        clear_time: now + Duration::from_secs(TOMBSTONE_DURATION_SECS),
                    };
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
}

// ---------------------------------------------------------------------------
// Convenience constructors for std
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
impl<I: Interface, const N: usize, const S: usize> Router<I, rand::rngs::StdRng, N, S> {
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
impl<I: Interface, const N: usize, const S: usize> Default for Router<I, rand::rngs::StdRng, N, S> {
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
            EDGE_NODE_ID => {}
            _ => {
                warn!(
                    "{}: device is sending us frames with a bad node id, ignoring",
                    frame.hdr
                );
                return;
            }
        }

        frame.hdr.src.network_id = net_id;
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
            // Send PROTOCOL_ERROR back to source with the bottleneck MTU
            let err_hdr = Header {
                src: hdr.dst.into(),
                dst: hdr.src.into(),
                any_all: None,
                seq_no: Some(hdr.seq_no),
                kind: crate::FrameKind::PROTOCOL_ERROR,
                ttl: crate::DEFAULT_TTL,
            };
            let _ = nsh
                .stack()
                .send_err(&err_hdr, ProtocolError::ISE_PACKET_TOO_BIG, Some(ident));
        }
        Err(e) => {
            warn!("{} recv->send error: {:?}", hdr, e);
        }
    }
}
