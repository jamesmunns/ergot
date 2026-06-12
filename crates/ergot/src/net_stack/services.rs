use embassy_futures::select::Either;

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::{
    interface_manager::Profile,
    net_stack::{NetStackHandle, endpoints::Endpoints, topics::Topics},
    socket::HeaderMessage,
    well_known::{
        AddressClaimGranted, AddressClaimRequest, AddressRefreshRequest, DeviceInfo,
        ErgotAddressClaimEndpoint, ErgotAddressRefreshEndpoint,
        ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoTopic, ErgotPingEndpoint,
        ErgotSeedRouterAssignmentEndpoint, ErgotSeedRouterRefreshEndpoint,
        ErgotSocketQueryResponseTopic, ErgotSocketQueryTopic, NameRequirement,
        SeedRouterAssignment, SeedRouterRefreshRequest, SocketQuery, SocketQueryResponse,
    },
};
use core::pin::pin;

use super::SocketHeaderIter;

/// A proxy type usable for creating helper services
pub struct Services<NS: NetStackHandle> {
    pub(super) inner: NS,
}

impl<NS: NetStackHandle> Services<NS> {
    /// Automatically responds to direct pings via the [`ErgotPingEndpoint`] endpoint
    ///
    /// The const parameter `D` controls the depth of the socket to buffer ping requests
    pub async fn ping_handler<const D: usize>(self) -> ! {
        let server = Endpoints {
            inner: self.inner.clone(),
        }
        .bounded_server::<ErgotPingEndpoint, D>(None);
        let server = pin!(server);
        let mut server_hdl = server.attach();
        loop {
            _ = server_hdl.serve_blocking(u32::clone).await;
        }
    }

    /// Handler for device info requests
    ///
    /// The const parameter `D` controls the depth of the socket to buffer info requests
    pub async fn device_info_handler<const D: usize>(self, info: &DeviceInfo) -> ! {
        let topics = Topics {
            inner: self.inner.clone(),
        };
        let subber = topics
            .clone()
            .bounded_receiver::<ErgotDeviceInfoInterrogationTopic, D>(None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe();

        loop {
            let msg = hdl.recv().await;
            let dest = msg.hdr.src;

            let _ = topics.clone().unicast::<ErgotDeviceInfoTopic>(dest, info);
        }
    }

    /// Handler for log messages that calls the given function for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn generic_log_handler<F>(self, depth: usize, f: F) -> !
    where
        F: Fn(HeaderMessage<ErgotFmtRxOwned>),
    {
        use crate::well_known::ErgotFmtRxOwnedTopic;

        let subber = Topics {
            inner: self.inner.clone(),
        }
        .heap_bounded_receiver::<ErgotFmtRxOwnedTopic>(depth, None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe();
        loop {
            let msg = hdl.recv().await;
            f(msg)
        }
    }

    /// Handler for unicast log messages that calls the given function for each received
    /// log message
    #[cfg(feature = "tokio-std")]
    pub async fn generic_log_handler_unicast<F>(
        self,
        depth: usize,
        f: F,
        notify_port: tokio::sync::oneshot::Sender<u8>,
    ) -> !
    where
        F: Fn(HeaderMessage<ErgotFmtRxOwned>),
    {
        use crate::well_known::ErgotFmtRxOwnedTopic;

        let subber = Topics {
            inner: self.inner.clone(),
        }
        .heap_bounded_receiver::<ErgotFmtRxOwnedTopic>(depth, None);

        let subber = pin!(subber);
        let mut hdl = subber.subscribe_unicast();
        _ = notify_port.send(hdl.port());
        loop {
            let msg = hdl.recv().await;
            f(msg)
        }
    }

    /// Handler for log messages that prints to the `log` crate sink for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn log_handler(self, depth: usize) -> ! {
        self.generic_log_handler(depth, log_fmtlog).await
    }

    /// Handler for unicast log messages that prints to the `log` crate sink for
    /// each received log message
    #[cfg(feature = "tokio-std")]
    pub async fn log_handler_unicast(
        self,
        depth: usize,
        notify_port: tokio::sync::oneshot::Sender<u8>,
    ) -> ! {
        self.generic_log_handler_unicast(depth, log_fmtlog, notify_port)
            .await
    }

    /// Handler for log messages that prints to stdout for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn default_stdout_log_handler(self, depth: usize) -> ! {
        self.generic_log_handler(depth, |msg| {
            println!(
                "({}.{}:{}) {:?}: {}",
                msg.hdr.src.network_id,
                msg.hdr.src.node_id,
                msg.hdr.src.port_id,
                msg.t.level,
                msg.t.inner,
            );
        })
        .await
    }

    /// Handler for accepting and responding to [`ErgotSocketQueryTopic`] messages
    pub async fn socket_query_handler<const D: usize>(self) {
        let nsh = self.inner.clone();
        let topics = Topics { inner: self.inner };
        let subber = topics
            .clone()
            .bounded_receiver::<ErgotSocketQueryTopic, D>(None);
        let subber = pin!(subber);
        let mut sub = subber.subscribe();
        loop {
            let msg = sub.recv().await;
            log::info!("{}: Got query!", msg.hdr);
            let res = nsh.stack().with_sockets(|iter| query_searcher(msg.t, iter));
            let Some(Some(resp)) = res else {
                continue;
            };
            log::info!("{}: Sending query response", msg.hdr);
            _ = topics
                .clone()
                .unicast::<ErgotSocketQueryResponseTopic>(msg.hdr.src, &resp);
        }
    }

    /// Handler for accepting and responding to Seed Router assignment and refresh requests
    ///
    /// Should only be used by Profiles that are capable of acting as Seed Routers, otherwise
    /// all requests will fail.
    pub async fn seed_router_request_handler<const D: usize>(self) {
        let nsh = self.inner.clone();
        let endpoints = Endpoints { inner: self.inner };

        let refresh = endpoints
            .clone()
            .bounded_server::<ErgotSeedRouterRefreshEndpoint, D>(None);
        let refresh = pin!(refresh);
        let mut refresh_svr = refresh.attach();
        let refresh_port = refresh_svr.port();

        let assign = endpoints
            .clone()
            .bounded_server::<ErgotSeedRouterAssignmentEndpoint, D>(None);
        let assign = pin!(assign);
        let mut assign_svr = assign.attach();

        loop {
            let res = embassy_futures::select::select(
                assign_svr.recv_manual(),
                refresh_svr.recv_manual(),
            )
            .await;
            match res {
                Either::First(assign_req) => {
                    let Ok(assign_req) = assign_req else {
                        continue;
                    };
                    handle_assign(&nsh, refresh_port, &assign_req)
                }
                Either::Second(refresh_req) => {
                    let Ok(refresh_req) = refresh_req else {
                        continue;
                    };
                    handle_refresh(&nsh, &refresh_req);
                }
            }
        }
    }
    /// Handler for accepting and responding to bus address claim and refresh requests.
    ///
    /// Should only be used by Profiles that support bus-style address claims (Router with C > 0).
    pub async fn address_claim_handler<const D: usize>(self) {
        let nsh = self.inner.clone();
        let endpoints = Endpoints { inner: self.inner };

        let refresh = endpoints
            .clone()
            .bounded_server::<ErgotAddressRefreshEndpoint, D>(None);
        let refresh = pin!(refresh);
        let mut refresh_svr = refresh.attach();
        let refresh_port = refresh_svr.port();

        let claim = endpoints
            .clone()
            .bounded_server::<ErgotAddressClaimEndpoint, D>(None);
        let claim = pin!(claim);
        let mut claim_svr = claim.attach();

        loop {
            let res = embassy_futures::select::select(
                claim_svr.recv_manual(),
                refresh_svr.recv_manual(),
            )
            .await;
            match res {
                Either::First(claim_req) => {
                    let Ok(claim_req) = claim_req else {
                        continue;
                    };
                    handle_address_claim(&nsh, refresh_port, &claim_req);
                }
                Either::Second(refresh_req) => {
                    let Ok(refresh_req) = refresh_req else {
                        continue;
                    };
                    handle_address_refresh(&nsh, &refresh_req);
                }
            }
        }
    }
}

/// Helper function for handling an address claim request
fn handle_address_claim<NS: NetStackHandle>(
    nsh: &NS,
    refresh_port: u8,
    req: &HeaderMessage<AddressClaimRequest>,
) {
    let res = nsh.stack().manage_profile(|p| {
        p.request_node_claim(
            req.hdr.src.network_id,
            req.t.candidate_node_id,
            req.t.nonce,
        )
    });
    let res = res.map(|assignment| AddressClaimGranted {
        assignment,
        refresh_port,
    });
    _ = nsh
        .stack()
        .endpoints()
        .respond_owned::<ErgotAddressClaimEndpoint>(&req.hdr, &res);
}

/// Helper function for handling an address refresh request
fn handle_address_refresh<NS: NetStackHandle>(
    nsh: &NS,
    req: &HeaderMessage<AddressRefreshRequest>,
) {
    let res = nsh.stack().manage_profile(|p| {
        p.refresh_node_claim(
            req.hdr.src.network_id,
            req.t.node_id,
            req.t.refresh_token,
        )
    });
    _ = nsh
        .stack()
        .endpoints()
        .respond_owned::<ErgotAddressRefreshEndpoint>(&req.hdr, &res);
}

/// log an ergot fmt log to log's global logger
#[cfg(feature = "std")]
fn log_fmtlog(msg: HeaderMessage<ErgotFmtRxOwned>) {
    use crate::fmtlog;
    match msg.t.level {
        fmtlog::Level::Error => log::error!(
            target: "remote_log",
            "({}.{}:{}): {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.inner
        ),
        fmtlog::Level::Warn => log::warn!(
            target: "remote_log",
            "({}.{}:{}): {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.inner
        ),
        fmtlog::Level::Info => log::info!(
            target: "remote_log",
            "({}.{}:{}): {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.inner
        ),
        fmtlog::Level::Debug => log::debug!(
            target: "remote_log",
            "({}.{}:{}): {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.inner
        ),
        fmtlog::Level::Trace => log::trace!(
            target: "remote_log",
            "({}.{}:{}): {}",
            msg.hdr.src.network_id,
            msg.hdr.src.node_id,
            msg.hdr.src.port_id,
            msg.t.inner
        ),
    }
}

/// Helper function for handling an Assign request
fn handle_assign<NS: NetStackHandle>(nsh: &NS, refresh_port: u8, assign_req: &HeaderMessage<()>) {
    let res = nsh
        .stack()
        .manage_profile(|p| p.request_seed_net_assign(assign_req.hdr.src.network_id));
    let res = res.map(|assignment| SeedRouterAssignment {
        assignment,
        refresh_port,
    });
    _ = nsh
        .stack()
        .endpoints()
        .respond_owned::<ErgotSeedRouterAssignmentEndpoint>(&assign_req.hdr, &res);
}

/// Helper function for handling a Refresh request
fn handle_refresh<NS: NetStackHandle>(
    nsh: &NS,
    refresh_req: &HeaderMessage<SeedRouterRefreshRequest>,
) {
    let res = nsh.stack().manage_profile(|p| {
        p.refresh_seed_net_assignment(
            refresh_req.hdr.src.network_id,
            refresh_req.t.refresh_net,
            refresh_req.t.refresh_token,
        )
    });
    _ = nsh
        .stack()
        .endpoints()
        .respond_owned::<ErgotSeedRouterRefreshEndpoint>(&refresh_req.hdr, &res);
}

// ---------------------------------------------------------------------------
// Bridge seed routing client
// ---------------------------------------------------------------------------

/// Result of a successful seed net_id assignment.
///
/// Contains all information needed to refresh the lease later.
#[derive(Debug, Clone)]
pub struct SeedLease {
    /// The assigned net_id for the downstream interface.
    pub net_id: u16,
    /// Address to send refresh requests to.
    pub refresh_addr: crate::Address,
    /// Current refresh token.
    pub refresh_token: [u8; 8],
    /// Lease duration in seconds.
    pub expires_seconds: u16,
    /// Maximum refresh interval in seconds.
    pub max_refresh_seconds: u16,
    /// Minimum time before expiration to refresh.
    pub min_refresh_seconds: u16,
}

/// Errors from bridge seed client operations.
#[derive(Debug)]
pub enum SeedClientError {
    /// The upstream interface is not Active.
    UpstreamNotActive,
    /// The seed router request failed.
    RequestFailed(super::ReqRespError),
    /// The seed router denied the assignment.
    AssignmentDenied(crate::interface_manager::SeedAssignmentError),
    /// The seed router denied the refresh.
    RefreshDenied(crate::interface_manager::SeedRefreshError),
    /// Failed to reassign the downstream interface net_id.
    ReassignFailed,
}

/// Request a seed net_id from the upstream router and assign it to a downstream interface.
///
/// The caller should ensure the upstream interface is Active before calling.
/// Returns a [`SeedLease`] for later refresh operations.
pub async fn bridge_seed_assign<NS: NetStackHandle + Clone>(
    nsh: &NS,
    upstream_ident: <NS::Profile as crate::interface_manager::Profile>::InterfaceIdent,
    downstream_ident: <NS::Profile as crate::interface_manager::Profile>::InterfaceIdent,
) -> Result<SeedLease, SeedClientError> {
    use crate::interface_manager::Profile;

    // 1. Get upstream net_id
    let upstream_net_id = nsh
        .stack()
        .manage_profile(|im| match im.interface_state(upstream_ident.clone()) {
            Some(crate::interface_manager::InterfaceState::Active { net_id, .. }) => Some(net_id),
            _ => None,
        })
        .ok_or(SeedClientError::UpstreamNotActive)?;

    // 2. Request seed assignment from upstream router (wildcard port)
    let upstream_addr = crate::Address {
        network_id: upstream_net_id,
        node_id: crate::interface_manager::edge_port::CENTRAL_NODE_ID,
        port_id: 0, // wildcard — find seed router by key
    };

    let endpoints = Endpoints { inner: nsh.clone() };
    let result = endpoints
        .request::<ErgotSeedRouterAssignmentEndpoint>(upstream_addr, &(), None)
        .await
        .map_err(SeedClientError::RequestFailed)?;

    let assignment = result.map_err(SeedClientError::AssignmentDenied)?;

    let seed_net_id = assignment.assignment.net_id;

    // 3. Reassign downstream interface net_id
    nsh.stack()
        .manage_profile(|im| im.reassign_interface_net_id(downstream_ident, seed_net_id))
        .map_err(|_| SeedClientError::ReassignFailed)?;

    // 4. Build refresh address
    let refresh_addr = crate::Address {
        network_id: upstream_net_id,
        node_id: crate::interface_manager::edge_port::CENTRAL_NODE_ID,
        port_id: assignment.refresh_port,
    };

    Ok(SeedLease {
        net_id: seed_net_id,
        refresh_addr,
        refresh_token: assignment.assignment.refresh_token,
        expires_seconds: assignment.assignment.expires_seconds,
        max_refresh_seconds: assignment.assignment.max_refresh_seconds,
        min_refresh_seconds: assignment.assignment.min_refresh_seconds,
    })
}

/// Refresh an existing seed net_id lease.
///
/// Returns an updated [`SeedLease`] on success.
pub async fn bridge_seed_refresh<NS: NetStackHandle + Clone>(
    nsh: &NS,
    lease: &SeedLease,
) -> Result<SeedLease, SeedClientError> {
    let result = Endpoints { inner: nsh.clone() }
        .request::<ErgotSeedRouterRefreshEndpoint>(
            lease.refresh_addr,
            &SeedRouterRefreshRequest {
                refresh_net: lease.net_id,
                refresh_token: lease.refresh_token,
            },
            None,
        )
        .await
        .map_err(SeedClientError::RequestFailed)?;

    let refreshed = result.map_err(SeedClientError::RefreshDenied)?;

    Ok(SeedLease {
        net_id: refreshed.net_id,
        refresh_addr: lease.refresh_addr,
        refresh_token: refreshed.refresh_token,
        expires_seconds: refreshed.expires_seconds,
        max_refresh_seconds: refreshed.max_refresh_seconds,
        min_refresh_seconds: refreshed.min_refresh_seconds,
    })
}

// ---------------------------------------------------------------------------
// Bus address claim client
// ---------------------------------------------------------------------------

/// A successful bus node_id claim, with everything needed to refresh it.
#[derive(Debug, Clone)]
pub struct NodeClaimLease {
    /// The claimed node_id.
    pub node_id: u8,
    /// The net_id of the bus segment.
    pub net_id: u16,
    /// Address to send refresh requests to.
    pub refresh_addr: crate::Address,
    /// Current refresh token.
    pub refresh_token: [u8; 8],
    /// Lease duration in seconds.
    pub expires_seconds: u16,
    /// Maximum refresh interval in seconds.
    pub max_refresh_seconds: u16,
    /// Minimum time before expiration to refresh.
    pub min_refresh_seconds: u16,
}

/// Errors from bus address claim client operations.
#[derive(Debug)]
pub enum ClaimClientError {
    /// The claim/refresh request failed at the req-resp layer.
    RequestFailed(super::ReqRespError),
    /// The router denied the claim.
    ClaimDenied(crate::interface_manager::AddressClaimError),
    /// The router denied the refresh.
    RefreshDenied(crate::interface_manager::AddressRefreshError),
    /// Failed to update the local interface state after a grant.
    SetStateFailed,
    /// Every candidate offered to [`bus_claim_with_retry`] was already taken.
    NoFreeCandidate,
}

/// Claim a node_id from the directly-attached router on a bus-style interface.
///
/// Sends an [`AddressClaimRequest`] to the router over link-local addressing
/// (`net_id = 0`, [`CENTRAL_NODE_ID`]) and, on success, sets `ident`'s state to
/// [`InterfaceState::Active`] with the granted address. Returns a
/// [`NodeClaimLease`] for later refresh.
///
/// `candidate_node_id` is the node_id the device would like; on
/// [`AddressClaimError::Conflict`] the caller should retry with a different
/// candidate.
///
/// [`CENTRAL_NODE_ID`]: crate::interface_manager::edge_port::CENTRAL_NODE_ID
/// [`InterfaceState::Active`]: crate::interface_manager::InterfaceState::Active
/// [`AddressClaimError::Conflict`]: crate::interface_manager::AddressClaimError::Conflict
pub async fn bus_claim<NS: NetStackHandle + Clone>(
    nsh: &NS,
    ident: <NS::Profile as crate::interface_manager::Profile>::InterfaceIdent,
    candidate_node_id: u8,
    nonce: u64,
) -> Result<NodeClaimLease, ClaimClientError> {
    use crate::interface_manager::Profile;

    let link_local = crate::Address {
        network_id: 0,
        node_id: crate::interface_manager::edge_port::CENTRAL_NODE_ID,
        port_id: 0, // wildcard — find the claim endpoint by key
    };

    let result = Endpoints { inner: nsh.clone() }
        .request::<ErgotAddressClaimEndpoint>(
            link_local,
            &AddressClaimRequest {
                candidate_node_id,
                nonce,
            },
            None,
        )
        .await
        .map_err(ClaimClientError::RequestFailed)?;

    let granted = result.map_err(ClaimClientError::ClaimDenied)?;
    let assignment = granted.assignment;

    nsh.stack()
        .manage_profile(|im| {
            im.set_interface_state(
                ident,
                crate::interface_manager::InterfaceState::Active {
                    net_id: assignment.net_id,
                    node_id: assignment.node_id,
                },
            )
        })
        .map_err(|_| ClaimClientError::SetStateFailed)?;

    let refresh_addr = crate::Address {
        network_id: assignment.net_id,
        node_id: crate::interface_manager::edge_port::CENTRAL_NODE_ID,
        port_id: granted.refresh_port,
    };

    Ok(NodeClaimLease {
        node_id: assignment.node_id,
        net_id: assignment.net_id,
        refresh_addr,
        refresh_token: assignment.refresh_token,
        expires_seconds: assignment.expires_seconds,
        max_refresh_seconds: assignment.max_refresh_seconds,
        min_refresh_seconds: assignment.min_refresh_seconds,
    })
}

/// Claim a node_id, trying each candidate in turn until one is granted.
///
/// Walks `candidates`, calling [`bus_claim`] for each. On
/// [`AddressClaimError::Conflict`] (the candidate is already taken on this
/// segment) it moves to the next candidate. A successful grant, or any other
/// error (a transport failure, a reserved/invalid candidate, or a failed local
/// state update — none of which a different candidate would fix), stops
/// immediately. Returns [`ClaimClientError::NoFreeCandidate`] if every
/// candidate was taken (or the iterator was empty).
///
/// `candidates` should be drawn from the claimable range (`3..=254`); pass a
/// range like `3..=254` for sequential probing, or an RNG-driven iterator for
/// randomized probing. Bound the attempt count by limiting the iterator (e.g.
/// `(3..=254).take(8)`). The same `nonce` is used for every attempt — it
/// identifies this device, so a lost response to a granted claim is recovered
/// idempotently rather than seen as a conflict.
///
/// [`AddressClaimError::Conflict`]: crate::interface_manager::AddressClaimError::Conflict
pub async fn bus_claim_with_retry<NS>(
    nsh: &NS,
    ident: <NS::Profile as crate::interface_manager::Profile>::InterfaceIdent,
    candidates: impl IntoIterator<Item = u8>,
    nonce: u64,
) -> Result<NodeClaimLease, ClaimClientError>
where
    NS: NetStackHandle + Clone,
    <NS::Profile as crate::interface_manager::Profile>::InterfaceIdent: Clone,
{
    use crate::interface_manager::AddressClaimError;

    for candidate in candidates {
        match bus_claim(nsh, ident.clone(), candidate, nonce).await {
            Ok(lease) => return Ok(lease),
            // Taken by another device — try the next candidate.
            Err(ClaimClientError::ClaimDenied(AddressClaimError::Conflict)) => continue,
            // Anything else won't be fixed by a different candidate.
            Err(e) => return Err(e),
        }
    }
    Err(ClaimClientError::NoFreeCandidate)
}

/// Refresh an existing bus node_id claim.
///
/// Returns an updated [`NodeClaimLease`] on success.
pub async fn bus_claim_refresh<NS: NetStackHandle + Clone>(
    nsh: &NS,
    lease: &NodeClaimLease,
) -> Result<NodeClaimLease, ClaimClientError> {
    let result = Endpoints { inner: nsh.clone() }
        .request::<ErgotAddressRefreshEndpoint>(
            lease.refresh_addr,
            &AddressRefreshRequest {
                node_id: lease.node_id,
                refresh_token: lease.refresh_token,
            },
            None,
        )
        .await
        .map_err(ClaimClientError::RequestFailed)?;

    let refreshed = result.map_err(ClaimClientError::RefreshDenied)?;

    Ok(NodeClaimLease {
        node_id: refreshed.node_id,
        net_id: refreshed.net_id,
        refresh_addr: lease.refresh_addr,
        refresh_token: refreshed.refresh_token,
        expires_seconds: refreshed.expires_seconds,
        max_refresh_seconds: refreshed.max_refresh_seconds,
        min_refresh_seconds: refreshed.min_refresh_seconds,
    })
}

/// Helper function for handling socket query requests
fn query_searcher(query: SocketQuery, iter: SocketHeaderIter) -> Option<SocketQueryResponse> {
    let SocketQuery {
        key,
        nash_req,
        frame_kind,
        broadcast,
    } = query;
    for hdr in iter {
        // Do cheaper comparisons first
        if frame_kind != hdr.attrs.kind {
            continue;
        }
        if broadcast && hdr.port != 255 {
            continue;
        }
        if !broadcast && hdr.port == 255 {
            continue;
        }
        match nash_req {
            NameRequirement::None => {
                if hdr.nash.is_some() {
                    continue;
                }
            }
            NameRequirement::Any => {}
            NameRequirement::Specific(name_hash) => {
                let Some(nash) = hdr.nash.as_ref() else {
                    continue;
                };
                if *nash != name_hash {
                    continue;
                }
            }
        }
        if key != hdr.key.0 {
            continue;
        }
        // all checks passed!
        return Some(SocketQueryResponse {
            name: hdr.nash,
            port: hdr.port,
        });
    }
    None
}
