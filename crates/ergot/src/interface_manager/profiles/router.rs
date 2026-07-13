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
        AddressClaimError, AddressRefreshError, DelegatedRefreshPreparation, Interface,
        InterfaceSendError, InterfaceState, NodeClaimAssignment, Profile, SeedAssignmentError,
        SeedLease, SeedNetAssignment, SeedRefreshError, SetStateError,
        edge_port::{CENTRAL_NODE_ID, EDGE_NODE_ID, EdgePort},
    },
    logging::{debug, trace, warn},
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

// These lease parameters are shared by seed-route and node-claim leases (both
// stored in `LeaseTable`), hence the representation-neutral names.
/// Initial lease duration for a newly granted seed net_id or node_id (seconds).
const INITIAL_LEASE_SECS: u16 = 30;
/// Maximum lease duration after refresh (seconds).
const MAX_LEASE_SECS: u16 = 120;
/// Refresh is allowed only when remaining time is less than this (seconds).
const MIN_REFRESH_SECS: u16 = 62;

/// Each delegation hop hands its downstream a `min_refresh_seconds` smaller
/// by this margin, so a child's refresh always lands inside the window where
/// the parent's own upstream refresh is accepted.
const SEED_DELEGATION_REFRESH_MARGIN: u16 = 5;

fn delegated_assignment(
    parent: &SeedLease,
    refresh_token: u64,
    expires_seconds: u16,
) -> SeedNetAssignment {
    SeedNetAssignment {
        net_id: parent.net_id,
        expires_seconds,
        max_refresh_seconds: parent.max_refresh_seconds,
        min_refresh_seconds: parent.min_refresh_seconds - SEED_DELEGATION_REFRESH_MARGIN,
        refresh_token: refresh_token.to_le_bytes(),
    }
}

fn remaining_lease_seconds(expiration: Instant, now: Instant) -> u16 {
    let remaining = expiration - now;
    let whole_seconds = remaining.as_secs();
    let rounded_up = whole_seconds.saturating_add(u64::from(
        remaining > Duration::from_secs(whole_seconds),
    ));
    rounded_up.min(u16::MAX as u64) as u16
}

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

// ---------------------------------------------------------------------------
// Lease lifecycle (representation-agnostic, shared by seed routes and claims)
// ---------------------------------------------------------------------------

/// A granted lease: when it expires, and the token required to refresh it.
#[derive(Clone, Copy)]
struct Lease {
    expiration: Instant,
    refresh_token: u64,
    /// The immediately previous token remains valid only for replaying a lost
    /// refresh response. A successful refresh replaces this replay slot.
    previous_refresh_token: Option<u64>,
}

/// The state of a leased resource.
#[derive(Clone, Copy)]
enum LeaseKind {
    /// An active lease.
    Active(Lease),
    /// Expired, but the key stays reserved until `clear_time` (the grace
    /// period) so it isn't reused while a stale peer might still use it.
    Tombstone { clear_time: Instant },
}

/// Why a [`LeaseKind::refresh`] was rejected.
enum RefreshDenied {
    /// Already expired (and now tombstoned).
    Expired,
    /// The presented refresh token doesn't match.
    BadToken,
    /// Too early to refresh.
    TooSoon,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenMatch {
    Current,
    Replay,
}

impl LeaseKind {
    /// A fresh active lease expiring `secs` from `now`.
    fn active(now: Instant, secs: u16, token: u64) -> Self {
        LeaseKind::Active(Lease {
            expiration: now + Duration::from_secs(secs as u64),
            refresh_token: token,
            previous_refresh_token: None,
        })
    }

    /// `true` if this is an active lease that hasn't expired yet.
    fn is_active(&self, now: Instant) -> bool {
        matches!(self, LeaseKind::Active(l) if l.expiration > now)
    }

    /// [`heapless::Vec::retain_mut`] predicate: an expired active lease becomes
    /// a tombstone (grace anchored to the expiration), and a tombstone whose
    /// grace period has elapsed is dropped.
    fn gc_retain(&mut self, now: Instant) -> bool {
        match *self {
            LeaseKind::Active(Lease { expiration, .. }) => {
                if now >= expiration {
                    let clear_time = expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS);
                    if now >= clear_time {
                        false
                    } else {
                        *self = LeaseKind::Tombstone { clear_time };
                        true
                    }
                } else {
                    true
                }
            }
            LeaseKind::Tombstone { clear_time } => clear_time > now,
        }
    }

    /// Validate a current refresh token or, when enabled, the immediately
    /// previous token used to replay a lost response. Expiry handling and
    /// token ordering live here for every seed state-machine path.
    fn validate_token(
        &mut self,
        req_token: u64,
        now: Instant,
        allow_replay: bool,
    ) -> Result<TokenMatch, RefreshDenied> {
        match self {
            LeaseKind::Tombstone { .. } => Err(RefreshDenied::Expired),
            LeaseKind::Active(lease) => {
                let token_match = if lease.refresh_token == req_token {
                    TokenMatch::Current
                } else if allow_replay && lease.previous_refresh_token == Some(req_token) {
                    TokenMatch::Replay
                } else {
                    return Err(RefreshDenied::BadToken);
                };
                if now >= lease.expiration {
                    *self = LeaseKind::Tombstone {
                        clear_time: lease.expiration + Duration::from_secs(TOMBSTONE_DURATION_SECS),
                    };
                    return Err(RefreshDenied::Expired);
                }
                Ok(token_match)
            }
        }
    }

    /// Refresh an active lease: verify the token, reject if expired (tombstoning
    /// it) or too soon, otherwise extend to `MAX_LEASE_SECS` and rotate
    /// the token to `new_token`. Returns the renewed lease and whether this
    /// was an idempotent replay rather than a new extension.
    fn refresh(
        &mut self,
        req_token: u64,
        now: Instant,
        new_token: u64,
        allow_replay: bool,
    ) -> Result<(Lease, bool), RefreshDenied> {
        let token_match = self.validate_token(req_token, now, allow_replay)?;
        let LeaseKind::Active(lease) = self else {
            unreachable!("successful validation guarantees an active lease")
        };
        if token_match == TokenMatch::Replay {
            return Ok((*lease, true));
        }
        if lease.expiration - now > Duration::from_secs(MIN_REFRESH_SECS as u64) {
            return Err(RefreshDenied::TooSoon);
        }
        lease.expiration = now + Duration::from_secs(MAX_LEASE_SECS as u64);
        lease.previous_refresh_token = allow_replay.then_some(lease.refresh_token);
        lease.refresh_token = new_token;
        Ok((*lease, false))
    }
}

/// One entry in a [`LeaseTable`]: the leased `key` (the value handed out), the
/// `scope` (net_id segment) it is valid on, a caller-specific `extra` payload,
/// and the lease state.
struct LeaseEntry<K, X> {
    key: K,
    scope: u16,
    extra: X,
    kind: LeaseKind,
}

/// A fixed-capacity table of leases, generic over the leased key type `K`
/// (a node_id or assigned net_id today; an address range under phone-number
/// addressing) and a caller-specific payload `X`.
struct LeaseTable<K, X, const N: usize> {
    entries: heapless::Vec<LeaseEntry<K, X>, N>,
}

impl<K: Copy + Eq, X, const N: usize> LeaseTable<K, X, N> {
    const fn new() -> Self {
        Self {
            entries: heapless::Vec::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.entries.is_full()
    }

    /// Tombstone expired leases and drop those past their grace period.
    fn gc(&mut self, now: Instant) {
        self.entries.retain_mut(|e| e.kind.gc_retain(now));
    }

    /// `true` if any entry currently holds `key` (active or tombstoned).
    fn contains_key(&self, key: K) -> bool {
        self.entries.iter().any(|e| e.key == key)
    }

    /// Check one key while lazily advancing or removing only that entry's
    /// lease state. This avoids a full-table GC on hot membership checks.
    fn contains_key_at(&mut self, key: K, now: Instant) -> bool {
        let Some(pos) = self.entries.iter().position(|entry| entry.key == key) else {
            return false;
        };
        if self.entries[pos].kind.gc_retain(now) {
            true
        } else {
            self.entries.swap_remove(pos);
            false
        }
    }

    /// Look up by key alone — for callers where `key` is globally unique
    /// (e.g. seed-assigned net_ids).
    fn by_key(&self, key: K) -> Option<&LeaseEntry<K, X>> {
        self.entries.iter().find(|e| e.key == key)
    }

    fn by_key_mut(&mut self, key: K) -> Option<&mut LeaseEntry<K, X>> {
        self.entries.iter_mut().find(|e| e.key == key)
    }

    /// Look up by `(key, scope)` — for keys only unique within a segment
    /// (e.g. a node_id, reused across buses).
    fn get(&self, key: K, scope: u16) -> Option<&LeaseEntry<K, X>> {
        self.entries.iter().find(|e| e.key == key && e.scope == scope)
    }

    fn get_mut(&mut self, key: K, scope: u16) -> Option<&mut LeaseEntry<K, X>> {
        self.entries
            .iter_mut()
            .find(|e| e.key == key && e.scope == scope)
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut LeaseEntry<K, X>> {
        self.entries.iter_mut()
    }

    /// Drop every entry in `scope`. Used when a segment's interface is
    /// removed: its leases (e.g. node_id claims keyed to that bus net_id)
    /// become meaningless and must not validate frames or block re-claims
    /// once the net_id is reused.
    fn drop_scope(&mut self, scope: u16) {
        self.entries.retain(|e| e.scope != scope);
    }

    /// Remove every entry with the given `key`. Used for idempotent
    /// re-registration of a globally-unique key (e.g. re-delegating a seed
    /// net_id this router already routes).
    fn remove_key(&mut self, key: K) {
        self.entries.retain(|e| e.key != key);
    }

    fn remove(&mut self, key: K, scope: u16) -> bool {
        let old_len = self.entries.len();
        self.entries
            .retain(|entry| entry.key != key || entry.scope != scope);
        self.entries.len() != old_len
    }

    /// Push a new entry. Returns `false` if the table is full.
    fn push(&mut self, key: K, scope: u16, extra: X, kind: LeaseKind) -> bool {
        self.entries
            .push(LeaseEntry {
                key,
                scope,
                extra,
                kind,
            })
            .is_ok()
    }
}

/// The upstream interface port (bridge mode only).
struct UpstreamPort<I: Interface> {
    port: EdgePort<I>,
    #[cfg(feature = "std")]
    closer: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
}

/// Routing metadata for one seed-assigned network.
struct SeedRoute {
    /// Direct downstream interface through which this network is reachable.
    via_ident: u8,
    /// Parent lease for delegated routes. Root-allocated routes have no
    /// parent because this router is their lease authority.
    parent: Option<SeedLease>,
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
    /// Seed-assigned routes. Key = assigned net_id, scope = requesting
    /// source net_id, extra = routing metadata and optional parent lease.
    seed_routes: LeaseTable<u16, SeedRoute, S>,
    /// Bus node_id claims. Key = node_id, scope = bus net_id, extra = nonce.
    node_claims: LeaseTable<u8, u64, C>,
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
    /// Bridge downlinks must start pending and receive a root-issued net_id.
    BridgeRequiresSeedAssignment,
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
        Self {
            slots: heapless::Vec::new(),
            seed_routes: LeaseTable::new(),
            node_claims: LeaseTable::new(),
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
            slots: heapless::Vec::new(),
            seed_routes: LeaseTable::new(),
            node_claims: LeaseTable::new(),
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
            || self.seed_routes.contains_key(net_id)
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
    /// removed by the seed-route GC. net_ids 0 and u16::MAX are reserved.
    /// Callers should run the relevant GC first so cleared tombstones release
    /// their net_ids.
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
        if self.has_upstream() {
            return Err(RegisterError::BridgeRequiresSeedAssignment);
        }
        // Reclaim net_ids from cleared seed-route tombstones before allocating.
        self.seed_routes.gc(Instant::now());
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
    /// Also tombstones any seed routes reachable through this interface, and
    /// drops any bus node_id claims scoped to its net_id (the segment is gone,
    /// and its net_id may be reused).
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

        // Tombstone seed routes reachable via this ident. The interface is
        // gone now, so the grace is anchored to now (no lease expiration).
        let clear_time = Instant::now() + Duration::from_secs(TOMBSTONE_DURATION_SECS);
        for e in self.seed_routes.iter_mut() {
            if e.extra.via_ident == ident {
                e.kind = LeaseKind::Tombstone { clear_time };
            }
        }

        // Drop node_id claims scoped to this interface's bus net_id. Unlike a
        // seed route (a globally-unique net_id, tombstoned so a stale peer
        // can't collide), a node claim is only meaningful while its bus exists;
        // once the interface is gone the net_id can be reused, and lingering
        // claims would validate frames or block re-claims on the new bus.
        self.node_claims.drop_scope(slot.net_id);

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
        self.seed_routes.gc(Instant::now());

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

        // 2. Seed route lookup (gc above already tombstoned expired routes).
        //    net_id is the unique key, so look up by key alone.
        let via_ident = match self.seed_routes.by_key(hdr.dst.network_id) {
            // 3. Upstream fallback (bridge mode)
            None => return self.find_upstream(source),
            Some(entry) if entry.kind.is_active(Instant::now()) => entry.extra.via_ident,
            Some(_) => return Err(InterfaceSendError::NoRouteToDest),
        };

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

/// Fold one broadcast-leg send result into the loop accumulators.
///
/// The benign class — down/inactive interface (`NoRouteToDest`), the frame's
/// own source (`RoutingLoop`), a self-addressed leg (`DestinationLocal`) —
/// just means "no recipient here" and is not remembered. Anything else (e.g.
/// `InterfaceFull`, `PacketTooBig`) is a *genuine* failure on an interface
/// that exists and was attempted; the caller reports it if no leg succeeded,
/// so a broadcast that failed everywhere is distinguishable from a broadcast
/// with no audience (see the conformance spec's "Broadcast Messages").
fn fold_broadcast_leg(
    res: Result<(), InterfaceSendError>,
    any_good: &mut bool,
    genuine: &mut Option<InterfaceSendError>,
) {
    match res {
        Ok(()) => *any_good = true,
        Err(
            InterfaceSendError::NoRouteToDest
            | InterfaceSendError::RoutingLoop
            | InterfaceSendError::DestinationLocal,
        ) => {}
        Err(e) => *genuine = Some(e),
    }
}

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
            let mut genuine = None;
            for slot in self.slots.iter_mut() {
                if hdr.dst.network_id == slot.net_id {
                    continue;
                }
                let mut bhdr = hdr.clone();
                bhdr.dst.network_id = slot.net_id;
                bhdr.dst.node_id = EDGE_NODE_ID;
                fold_broadcast_leg(slot.port.send(&bhdr, data), &mut any_good, &mut genuine);
            }
            // Also broadcast to upstream (bridge mode)
            if let Some(up) = self.upstream.as_mut() {
                fold_broadcast_leg(up.port.send(&hdr, data), &mut any_good, &mut genuine);
            }
            if any_good {
                Ok(())
            } else if let Some(e) = genuine {
                Err(e)
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
            let mut genuine = None;

            for slot in self.slots.iter_mut() {
                if source == slot.ident {
                    continue;
                }
                default_error = InterfaceSendError::NoRouteToDest;

                hdr.dst.network_id = slot.net_id;
                hdr.dst.node_id = EDGE_NODE_ID;
                fold_broadcast_leg(slot.port.send_raw(&hdr, data), &mut any_good, &mut genuine);
            }
            // Also broadcast to upstream (bridge mode), unless source is upstream
            if let Some(up) = self.upstream.as_mut()
                && source != UPSTREAM_IDENT
            {
                default_error = InterfaceSendError::NoRouteToDest;
                fold_broadcast_leg(up.port.send_raw(&hdr, data), &mut any_good, &mut genuine);
            }
            if any_good {
                Ok(())
            } else if let Some(e) = genuine {
                Err(e)
            } else {
                Err(default_error)
            }
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
        if new_net_id == 0
            || self
                .slots
                .iter()
                .any(|slot| slot.ident != ident && slot.net_id == new_net_id)
            || self.seed_routes.contains_key(new_net_id)
            || self
                .upstream
                .as_ref()
                .is_some_and(|up| up.port.net_id() == Some(new_net_id))
        {
            return Err(SetStateError::NetIdInUse);
        }
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
        if self.has_upstream() {
            return Err(SeedAssignmentError::ProfileCantSeed);
        }
        let now = Instant::now();
        self.seed_routes.gc(now);

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
        self.seed_routes.push(
            net_id,
            source_net,
            SeedRoute {
                via_ident,
                parent: None,
            },
            LeaseKind::active(now, INITIAL_LEASE_SECS, refresh_token),
        );

        Ok(SeedNetAssignment {
            net_id,
            expires_seconds: INITIAL_LEASE_SECS,
            max_refresh_seconds: MAX_LEASE_SECS,
            min_refresh_seconds: MIN_REFRESH_SECS,
            refresh_token: refresh_token.to_le_bytes(),
        })
    }

    fn seed_delegation_upstream(&self) -> Option<Self::InterfaceIdent> {
        if self.has_upstream() {
            Some(UPSTREAM_IDENT)
        } else {
            None
        }
    }

    fn can_delegate_seed(&mut self, source_net: u16) -> Result<(), SeedAssignmentError> {
        self.seed_routes.gc(Instant::now());
        if source_net == 0 || !self.slots.iter().any(|s| s.net_id == source_net) {
            return Err(SeedAssignmentError::UnknownSource);
        }
        if self.seed_routes.is_full() {
            return Err(SeedAssignmentError::NetIdsExhausted);
        }
        Ok(())
    }

    fn register_delegated_seed_net(
        &mut self,
        source_net: u16,
        parent: &SeedLease,
    ) -> Result<SeedNetAssignment, SeedAssignmentError> {
        let now = Instant::now();
        self.seed_routes.gc(now);

        if source_net == 0 {
            return Err(SeedAssignmentError::UnknownSource);
        }

        let via_ident = self
            .slots
            .iter()
            .find(|s| s.net_id == source_net)
            .map(|s| s.ident)
            .ok_or(SeedAssignmentError::UnknownSource)?;

        if self.slots.iter().any(|s| s.net_id == parent.net_id)
            || self
                .upstream
                .as_ref()
                .is_some_and(|up| up.port.net_id() == Some(parent.net_id))
        {
            return Err(SeedAssignmentError::NetIdCollision);
        }

        if parent.min_refresh_seconds <= SEED_DELEGATION_REFRESH_MARGIN {
            return Err(SeedAssignmentError::DelegationDepthExceeded);
        }

        // Re-delegation of a net we already route is idempotent: drop the
        // stale entry and re-register. (net_id is the table's unique key.)
        if let Some(existing) = self.seed_routes.by_key(parent.net_id)
            && existing.scope != source_net
            && existing.kind.is_active(now)
        {
            warn!(
                "Replacing active seed route net_id {} owned by source net {} with source net {}",
                parent.net_id, existing.scope, source_net
            );
        }
        self.seed_routes.remove_key(parent.net_id);
        if self.seed_routes.is_full() {
            return Err(SeedAssignmentError::NetIdsExhausted);
        }

        // The delegated route's lease tracks the upstream lease we hold, so it
        // expires when the upstream lease does.
        let refresh_token = self.rng.next_u64();
        self.seed_routes.push(
            parent.net_id,
            source_net,
            SeedRoute {
                via_ident,
                parent: Some(parent.clone()),
            },
            LeaseKind::active(now, parent.expires_seconds, refresh_token),
        );

        Ok(delegated_assignment(
            parent,
            refresh_token,
            parent.expires_seconds,
        ))
    }

    fn prepare_delegated_refresh(
        &mut self,
        source_net: u16,
        refresh_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<DelegatedRefreshPreparation, SeedRefreshError> {
        let now = Instant::now();
        self.seed_routes.gc(now);
        let req_token = u64::from_le_bytes(refresh_token);
        // net_id is the unique key; the requester (source_net) is the scope.
        let entry = self
            .seed_routes
            .by_key_mut(refresh_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;
        if entry.scope != source_net {
            return Err(SeedRefreshError::BadRequest);
        }
        match entry.kind.validate_token(req_token, now, true) {
            Err(RefreshDenied::Expired) => Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken | RefreshDenied::TooSoon) => {
                Err(SeedRefreshError::BadRequest)
            }
            Ok(TokenMatch::Replay) => {
                let LeaseKind::Active(lease) = entry.kind else {
                    unreachable!("successful validation guarantees an active lease")
                };
                let parent = entry
                    .extra
                    .parent
                    .as_ref()
                    .ok_or(SeedRefreshError::NotAssigned)?;
                Ok(DelegatedRefreshPreparation::Replay(delegated_assignment(
                    parent,
                    lease.refresh_token,
                    remaining_lease_seconds(lease.expiration, now),
                )))
            }
            Ok(TokenMatch::Current) => entry
                .extra
                .parent
                .clone()
                .map(DelegatedRefreshPreparation::Forward)
                .ok_or(SeedRefreshError::NotAssigned),
        }
    }

    fn commit_delegated_refresh(
        &mut self,
        source_net: u16,
        refresh_token: [u8; 8],
        refreshed_parent: &SeedLease,
    ) -> Result<SeedNetAssignment, SeedRefreshError> {
        if refreshed_parent.min_refresh_seconds <= SEED_DELEGATION_REFRESH_MARGIN {
            return Err(SeedRefreshError::DelegationDepthExceeded);
        }
        let req_token = u64::from_le_bytes(refresh_token);
        let new_token = self.rng.next_u64();
        let now = Instant::now();

        let entry = self
            .seed_routes
            .get_mut(refreshed_parent.net_id, source_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;

        match entry.kind.validate_token(req_token, now, false) {
            Err(RefreshDenied::Expired) => Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken | RefreshDenied::TooSoon) => {
                Err(SeedRefreshError::BadRequest)
            }
            Ok(_) => {
                let LeaseKind::Active(lease) = &mut entry.kind else {
                    unreachable!("successful validation guarantees an active lease")
                };
                // No TooSoon check here: pacing is enforced by the upstream
                // seed router, whose refresh has already succeeded. Extend to
                // track the upstream lease and rotate the local token.
                let parent = entry
                    .extra
                    .parent
                    .as_mut()
                    .ok_or(SeedRefreshError::NotAssigned)?;
                *parent = refreshed_parent.clone();
                lease.expiration =
                    now + Duration::from_secs(refreshed_parent.expires_seconds as u64);
                lease.previous_refresh_token = Some(lease.refresh_token);
                lease.refresh_token = new_token;
                Ok(delegated_assignment(
                    refreshed_parent,
                    new_token,
                    refreshed_parent.expires_seconds,
                ))
            }
        }
    }

    fn prepare_delegated_release(
        &mut self,
        source_net: u16,
        release_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<SeedLease, SeedRefreshError> {
        let now = Instant::now();
        self.seed_routes.gc(now);
        let req_token = u64::from_le_bytes(refresh_token);
        let entry = self
            .seed_routes
            .get_mut(release_net, source_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;
        match entry.kind.validate_token(req_token, now, false) {
            Err(RefreshDenied::Expired) => Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken | RefreshDenied::TooSoon) => {
                Err(SeedRefreshError::BadRequest)
            }
            Ok(_) => entry
                .extra
                .parent
                .clone()
                .ok_or(SeedRefreshError::NotAssigned),
        }
    }

    fn commit_delegated_release(
        &mut self,
        source_net: u16,
        release_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<(), SeedRefreshError> {
        let req_token = u64::from_le_bytes(refresh_token);
        let entry = self
            .seed_routes
            .get_mut(release_net, source_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;
        match entry.kind.validate_token(req_token, Instant::now(), false) {
            Err(RefreshDenied::Expired) => return Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken | RefreshDenied::TooSoon) => {
                return Err(SeedRefreshError::BadRequest);
            }
            Ok(_) => {}
        }
        self.seed_routes.remove(release_net, source_net);
        Ok(())
    }

    fn release_seed_net_assignment(
        &mut self,
        source_net: u16,
        release_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<(), SeedRefreshError> {
        let now = Instant::now();
        self.seed_routes.gc(now);
        let req_token = u64::from_le_bytes(refresh_token);
        let entry = self
            .seed_routes
            .get_mut(release_net, source_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;
        match entry.kind.validate_token(req_token, now, false) {
            Err(RefreshDenied::Expired) => return Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken | RefreshDenied::TooSoon) => {
                return Err(SeedRefreshError::BadRequest);
            }
            Ok(_) => {}
        }
        self.seed_routes.remove(release_net, source_net);
        Ok(())
    }

    fn refresh_seed_net_assignment(
        &mut self,
        source_net: u16,
        refresh_net: u16,
        refresh_token: [u8; 8],
    ) -> Result<SeedNetAssignment, SeedRefreshError> {
        let req_token = u64::from_le_bytes(refresh_token);
        // Pre-generate the new token before borrowing seed_routes.
        let new_token = self.rng.next_u64();
        let now = Instant::now();

        // A seed route is keyed by (assigned net_id, requesting source_net); a
        // mismatch on either means the requester doesn't own this lease.
        let entry = self
            .seed_routes
            .get_mut(refresh_net, source_net)
            .ok_or(SeedRefreshError::UnknownNetId)?;

        match entry.kind.refresh(req_token, now, new_token, true) {
            Ok((lease, replayed)) => Ok(SeedNetAssignment {
                net_id: refresh_net,
                expires_seconds: if replayed {
                    remaining_lease_seconds(lease.expiration, now)
                } else {
                    MAX_LEASE_SECS
                },
                max_refresh_seconds: MAX_LEASE_SECS,
                min_refresh_seconds: MIN_REFRESH_SECS,
                refresh_token: lease.refresh_token.to_le_bytes(),
            }),
            Err(RefreshDenied::Expired) => Err(SeedRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken) => Err(SeedRefreshError::BadRequest),
            Err(RefreshDenied::TooSoon) => Err(SeedRefreshError::TooSoon),
        }
    }

    fn request_node_claim(
        &mut self,
        source_net: u16,
        candidate: u8,
        nonce: u64,
    ) -> Result<NodeClaimAssignment, AddressClaimError> {
        // Reject reserved node_ids: 0 ("any"), CENTRAL/EDGE (point-to-point
        // roles that are always valid), and 255 (broadcast).
        if matches!(candidate, 0 | CENTRAL_NODE_ID | EDGE_NODE_ID | 255) {
            return Err(AddressClaimError::InvalidNodeId);
        }

        // GC expired claims first.
        let now = Instant::now();
        self.node_claims.gc(now);

        // Verify source net_id belongs to a known interface.
        if !self.slots.iter().any(|s| s.net_id == source_net) {
            return Err(AddressClaimError::UnknownSource);
        }

        // Check if the candidate is already claimed on this bus.
        if let Some(entry) = self.node_claims.get(candidate, source_net) {
            return match entry.kind {
                // Same nonce = retransmit from the same device: idempotently
                // return the existing assignment. Different nonce or a
                // tombstone = the node_id is taken/reserved → conflict.
                LeaseKind::Active(lease) if entry.extra == nonce => Ok(NodeClaimAssignment {
                    node_id: candidate,
                    net_id: source_net,
                    expires_seconds: lease.expiration.saturating_duration_since(now).as_secs()
                        as u16,
                    max_refresh_seconds: MAX_LEASE_SECS,
                    min_refresh_seconds: MIN_REFRESH_SECS,
                    refresh_token: lease.refresh_token.to_le_bytes(),
                }),
                _ => Err(AddressClaimError::Conflict),
            };
        }

        if self.node_claims.is_full() {
            return Err(AddressClaimError::Exhausted);
        }

        let refresh_token = self.rng.next_u64();
        self.node_claims.push(
            candidate,
            source_net,
            nonce,
            LeaseKind::active(now, INITIAL_LEASE_SECS, refresh_token),
        );

        Ok(NodeClaimAssignment {
            node_id: candidate,
            net_id: source_net,
            expires_seconds: INITIAL_LEASE_SECS,
            max_refresh_seconds: MAX_LEASE_SECS,
            min_refresh_seconds: MIN_REFRESH_SECS,
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
        let now = Instant::now();

        let entry = self
            .node_claims
            .get_mut(node_id, source_net)
            .ok_or(AddressRefreshError::UnknownNodeId)?;

        match entry.kind.refresh(req_token, now, new_token, false) {
            Ok((lease, _)) => Ok(NodeClaimAssignment {
                node_id,
                net_id: source_net,
                expires_seconds: MAX_LEASE_SECS,
                max_refresh_seconds: MAX_LEASE_SECS,
                min_refresh_seconds: MIN_REFRESH_SECS,
                refresh_token: lease.refresh_token.to_le_bytes(),
            }),
            Err(RefreshDenied::Expired) => Err(AddressRefreshError::AlreadyExpired),
            Err(RefreshDenied::BadToken) => Err(AddressRefreshError::BadRequest),
            Err(RefreshDenied::TooSoon) => Err(AddressRefreshError::TooSoon),
        }
    }

    fn is_node_claimed(&mut self, net_id: u16, node_id: u8) -> bool {
        // CENTRAL/EDGE are always valid for point-to-point compatibility.
        if node_id == CENTRAL_NODE_ID || node_id == EDGE_NODE_ID {
            return true;
        }
        // Scoped to the net_id the frame arrived on: a claim only validates
        // frames on its own bus segment. is_active() rejects an expired claim
        // immediately, so a quiet bus can't keep a stale node_id alive.
        self.node_claims
            .get(node_id, net_id)
            .is_some_and(|e| e.kind.is_active(Instant::now()))
    }

    fn is_transit_net(&mut self, net_id: u16) -> bool {
        if net_id == 0 {
            return false;
        }
        // Direct downstream segments (pending slots hold net_id=0 and are
        // excluded by the check above) and seed-assigned routes. Tombstoned
        // seed routes count too: a recently expired downstream net is still
        // known-not-ours and must not be adopted as the upstream's own.
        self.slots.iter().any(|s| s.net_id == net_id)
            || self.seed_routes.contains_key_at(net_id, Instant::now())
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

#[cfg(all(test, feature = "tokio-std"))]
mod tests {
    use super::*;
    use crate::interface_manager::interface_impls::tokio_stream::TokioStreamInterface;
    use rand::{SeedableRng, rngs::StdRng};

    #[test]
    fn transit_check_collects_seed_tombstones_after_grace() {
        const NET_ID: u16 = 42;
        let mut router: Router<TokioStreamInterface, StdRng, 1, 1> =
            Router::new(StdRng::seed_from_u64(0));

        assert!(router.seed_routes.push(
            NET_ID,
            1,
            SeedRoute {
                via_ident: 0,
                parent: None,
            },
            LeaseKind::Tombstone {
                clear_time: Instant::now() + Duration::from_secs(60),
            },
        ));
        assert!(
            router.is_transit_net(NET_ID),
            "a tombstone inside its grace period must remain transit"
        );

        router.seed_routes.entries[0].kind = LeaseKind::Tombstone {
            clear_time: Instant::now(),
        };
        assert!(
            !router.is_transit_net(NET_ID),
            "a tombstone past its grace period must not block upstream rediscovery"
        );
        assert!(router.seed_routes.entries.is_empty());
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

    // Bus address claim validation: a device on this segment must have claimed
    // its node_id before it may send. This applies only to frames *originating*
    // on this segment — after the zero-rewrite above, those have
    // `src.network_id == net_id`. A transit frame routed through this router
    // carries a foreign src net_id and a node_id this router does not
    // arbitrate, so it must not be validated here (doing so breaks multi-hop
    // routing of bus-originated traffic). Wildcard-port frames (port_id=0) are
    // also exempt: they may be claim requests from a node without an address
    // yet.
    if frame.hdr.dst.port_id != 0 && frame.hdr.src.network_id == net_id {
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
