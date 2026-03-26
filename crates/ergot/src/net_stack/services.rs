use embassy_futures::select::Either;

#[cfg(feature = "std")]
use crate::fmtlog::ErgotFmtRxOwned;
use crate::{
    interface_manager::Profile,
    net_stack::{NetStackHandle, endpoints::Endpoints, topics::Topics},
    socket::HeaderMessage,
    well_known::{
        DeviceInfo, ErgotDeviceInfoInterrogationTopic, ErgotDeviceInfoTopic, ErgotPingEndpoint,
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

    /// Handler for log messages that prints to the `log` crate sink for each received
    /// log message
    #[cfg(feature = "std")]
    pub async fn log_handler(self, depth: usize) -> ! {
        use crate::fmtlog;

        self.generic_log_handler(depth, |msg| match msg.t.level {
            fmtlog::Level::Error => log::error!(
                target: "remote_log",
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Warn => log::warn!(
                target: "remote_log",
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Info => log::info!(
                target: "remote_log",
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Debug => log::debug!(
                target: "remote_log",
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Trace => log::trace!(
                target: "remote_log",
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
        })
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
