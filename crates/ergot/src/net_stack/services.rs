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
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Warn => log::warn!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Info => log::info!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Debug => log::debug!(
                "({}.{}:{}): {}",
                msg.hdr.src.network_id,
                msg.hdr.src.network_id,
                msg.hdr.src.port_id,
                msg.t.inner
            ),
            fmtlog::Level::Trace => log::trace!(
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
            log::info!("Got query!");
            let res = nsh.stack().with_sockets(|iter| query_searcher(msg.t, iter));
            let Some(Some(resp)) = res else {
                continue;
            };
            log::info!("Sending query response to {:?}", msg.hdr.src);
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
