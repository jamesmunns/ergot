//! Regression tests for delegated seed failure handling.

#![cfg(feature = "tokio-std")]
#![cfg(not(miri))]

use std::time::Duration;

use ergot::{
    Address, HeaderSeq, ProtocolError,
    interface_manager::{
        Interface, InterfaceSink, InterfaceState, Profile, SeedAssignmentError, SeedLease,
        SeedRefreshError, SetStateError,
        interface_impls::tokio_stream::TokioStreamInterface,
        profiles::router::{Router, UPSTREAM_IDENT},
    },
    net_stack::{
        ArcNetStack,
        services::{SeedClientError, bridge_seed_refresh, request_seed_lease},
    },
    well_known::{
        ErgotPingEndpoint, ErgotSeedRouterAssignmentEndpoint, ErgotSeedRouterReleaseEndpoint,
        SeedRouterAssignment,
    },
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use rand::SeedableRng;
use serde::Serialize;
use tokio::time::{sleep, timeout};

#[derive(Clone)]
struct NullSink;

impl InterfaceSink for NullSink {
    fn mtu(&self) -> u16 {
        2048
    }

    fn send_ty<T: Serialize>(&mut self, _: &HeaderSeq, _: &T) -> Result<(), ()> {
        Ok(())
    }

    fn send_raw(&mut self, _: &HeaderSeq, _: &[u8]) -> Result<(), ()> {
        Ok(())
    }

    fn send_err(&mut self, _: &HeaderSeq, _: ProtocolError) -> Result<(), ()> {
        Ok(())
    }
}

struct MockInterface;

impl Interface for MockInterface {
    type Sink = NullSink;
}

type TestRouter = Router<MockInterface, rand::rngs::StdRng, 8, 8, 4>;
type TestStack = ArcNetStack<CriticalSectionRawMutex, TestRouter>;

fn rng(seed: u8) -> rand::rngs::StdRng {
    rand::rngs::StdRng::from_seed([seed; 32])
}

#[test]
fn upstream_refresh_retry_after_lost_response_is_idempotent() {
    let root: TestStack = TestStack::new_with_profile(Router::new(rng(1)));
    let root_down = root
        .manage_profile(|im| im.register_interface(NullSink))
        .unwrap();
    let root_source_net = root.manage_profile(|im| im.net_id_of(root_down)).unwrap();

    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(2), NullSink));
    let bridge_down = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    let bridge_link_assignment = root
        .manage_profile(|im| im.request_seed_net_assign(root_source_net))
        .unwrap();
    bridge
        .manage_profile(|im| {
            im.reassign_interface_net_id(bridge_down, bridge_link_assignment.net_id)
        })
        .unwrap();
    let bridge_source_net = bridge
        .manage_profile(|im| im.net_id_of(bridge_down))
        .unwrap();

    let parent_assignment = root
        .manage_profile(|im| im.request_seed_net_assign(root_source_net))
        .unwrap();
    let parent = SeedLease {
        net_id: parent_assignment.net_id,
        refresh_addr: Address {
            network_id: root_source_net,
            node_id: 1,
            port_id: 42,
        },
        release_addr: Address {
            network_id: root_source_net,
            node_id: 1,
            port_id: 43,
        },
        refresh_token: parent_assignment.refresh_token,
        expires_seconds: parent_assignment.expires_seconds,
        max_refresh_seconds: parent_assignment.max_refresh_seconds,
        min_refresh_seconds: parent_assignment.min_refresh_seconds,
    };
    let child_assignment = bridge
        .manage_profile(|im| im.register_delegated_seed_net(bridge_source_net, &parent))
        .unwrap();

    let prepared = bridge
        .manage_profile(|im| {
            im.prepare_delegated_refresh(
                bridge_source_net,
                child_assignment.net_id,
                child_assignment.refresh_token,
            )
        })
        .unwrap();
    let ergot::interface_manager::DelegatedRefreshPreparation::Forward(prepared) = prepared else {
        panic!("the first refresh must be forwarded upstream");
    };
    let first_response = root
        .manage_profile(|im| {
            im.refresh_seed_net_assignment(root_source_net, prepared.net_id, prepared.refresh_token)
        })
        .unwrap();

    // Simulate losing that response (or cancelling the bridge handler) before
    // commit. After restart the bridge can only retry the old parent token.
    let prepared_after_restart = bridge
        .manage_profile(|im| {
            im.prepare_delegated_refresh(
                bridge_source_net,
                child_assignment.net_id,
                child_assignment.refresh_token,
            )
        })
        .unwrap();
    let ergot::interface_manager::DelegatedRefreshPreparation::Forward(prepared_after_restart) =
        prepared_after_restart
    else {
        panic!("an uncommitted refresh must still be forwarded after restart");
    };
    let retry = root.manage_profile(|im| {
        im.refresh_seed_net_assignment(
            root_source_net,
            prepared_after_restart.net_id,
            prepared_after_restart.refresh_token,
        )
    });

    assert_eq!(
        retry,
        Ok(first_response),
        "retrying after a lost refresh response must be idempotent"
    );
}

#[test]
fn replay_reports_actual_remaining_lease_time() {
    let root: TestStack = TestStack::new_with_profile(Router::new(rng(8)));
    let down = root
        .manage_profile(|im| im.register_interface(NullSink))
        .unwrap();
    let source_net = root.manage_profile(|im| im.net_id_of(down)).unwrap();
    let initial = root
        .manage_profile(|im| im.request_seed_net_assign(source_net))
        .unwrap();
    let refreshed = root
        .manage_profile(|im| {
            im.refresh_seed_net_assignment(source_net, initial.net_id, initial.refresh_token)
        })
        .unwrap();

    std::thread::sleep(Duration::from_millis(1_100));

    let replay = root
        .manage_profile(|im| {
            im.refresh_seed_net_assignment(source_net, initial.net_id, initial.refresh_token)
        })
        .unwrap();
    assert_eq!(replay.refresh_token, refreshed.refresh_token);
    assert!(
        replay.expires_seconds < refreshed.expires_seconds,
        "a replay must not restart the downstream's 120-second clock"
    );
}

#[test]
fn delegation_rejects_a_hop_that_would_exhaust_the_refresh_margin() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(3), NullSink));
    let bridge_down = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    bridge
        .manage_profile(|im| im.reassign_interface_net_id(bridge_down, 1))
        .unwrap();
    let source_net = bridge
        .manage_profile(|im| im.net_id_of(bridge_down))
        .unwrap();
    let parent = SeedLease {
        net_id: 42,
        refresh_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 42,
        },
        release_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 43,
        },
        refresh_token: [0xAA; 8],
        expires_seconds: 120,
        max_refresh_seconds: 120,
        min_refresh_seconds: 5,
    };

    assert_eq!(
        bridge.manage_profile(|im| im.register_delegated_seed_net(source_net, &parent)),
        Err(SeedAssignmentError::DelegationDepthExceeded)
    );
}

#[test]
fn bridge_requires_pending_root_assigned_downlinks() {
    use ergot::interface_manager::profiles::router::RegisterError;

    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(4), NullSink));
    assert_eq!(
        bridge.manage_profile(|im| im.register_interface(NullSink)),
        Err(RegisterError::BridgeRequiresSeedAssignment)
    );

    let pending = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    assert_eq!(bridge.manage_profile(|im| im.net_id_of(pending)), Some(0));
}

#[test]
fn bridge_rejects_zero_source_and_local_seed_allocation() {
    let bridge: TestStack = TestStack::new_with_profile(Router::new_bridge(rng(5), NullSink));
    let pending = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();

    assert_eq!(
        bridge.manage_profile(|im| im.can_delegate_seed(0)),
        Err(SeedAssignmentError::UnknownSource)
    );
    assert_eq!(
        bridge.manage_profile(|im| im.request_seed_net_assign(0)),
        Err(SeedAssignmentError::ProfileCantSeed)
    );

    bridge
        .manage_profile(|im| im.reassign_interface_net_id(pending, 7))
        .unwrap();
    let colliding_parent = SeedLease {
        net_id: 7,
        refresh_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 42,
        },
        release_addr: Address {
            network_id: 1,
            node_id: 1,
            port_id: 43,
        },
        refresh_token: [0xBB; 8],
        expires_seconds: 120,
        max_refresh_seconds: 120,
        min_refresh_seconds: 62,
    };
    assert_eq!(
        bridge.manage_profile(|im| im.register_delegated_seed_net(7, &colliding_parent)),
        Err(SeedAssignmentError::NetIdCollision)
    );

    let other = bridge
        .manage_profile(|im| im.register_interface_pending(NullSink))
        .unwrap();
    assert_eq!(
        bridge.manage_profile(|im| im.reassign_interface_net_id(other, 7)),
        Err(SetStateError::NetIdInUse)
    );
}

type RouterStack =
    ArcNetStack<CriticalSectionRawMutex, Router<TokioStreamInterface, rand::rngs::StdRng, 64, 64>>;

async fn wait_interface_active(stack: &RouterStack, ident: u8) -> u16 {
    for _ in 0..60 {
        if let Some(InterfaceState::Active { net_id, .. }) =
            stack.manage_profile(|im| im.interface_state(ident))
        {
            return net_id;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("interface {ident} never became Active");
}

async fn setup_delegation_chain() -> (RouterStack, RouterStack, RouterStack) {
    use ergot::interface_manager::transports::tokio_cobs_stream as tcs;
    use ergot::interface_manager::utils::{cobs_stream, std::new_std_queue};

    let root: RouterStack = RouterStack::new();

    let bridge_up_queue = new_std_queue(4096);
    let bridge: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(bridge_up_queue.clone(), 512),
    ));

    let requester_up_queue = new_std_queue(4096);
    let requester: RouterStack = RouterStack::new_with_profile(Router::new_bridge_std(
        cobs_stream::Sink::new_from_handle(requester_up_queue.clone(), 512),
    ));

    let (bridge_up_read, root_b_write) = tokio::io::duplex(8192);
    let (root_b_read, bridge_up_write) = tokio::io::duplex(8192);
    tcs::register_router(
        root.clone(),
        root_b_read,
        root_b_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();
    tcs::register_bridge_upstream(
        bridge.clone(),
        bridge_up_read,
        bridge_up_write,
        bridge_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    let _ = timeout(
        Duration::from_millis(500),
        root.endpoints().request::<ErgotPingEndpoint>(
            Address {
                network_id: 1,
                node_id: 2,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(wait_interface_active(&bridge, UPSTREAM_IDENT).await, 1);

    let (req_up_read, bridge_r_write) = tokio::io::duplex(8192);
    let (bridge_r_read, req_up_write) = tokio::io::duplex(8192);
    let bridge_down = tcs::register_bridge_downstream(
        bridge.clone(),
        bridge_r_read,
        bridge_r_write,
        512,
        4096,
        None,
        None,
    )
    .await
    .unwrap();
    let bridge_link_assignment = root
        .manage_profile(|im| im.request_seed_net_assign(1))
        .unwrap();
    bridge
        .manage_profile(|im| {
            im.reassign_interface_net_id(bridge_down, bridge_link_assignment.net_id)
        })
        .unwrap();
    let bridge_down_net = bridge
        .manage_profile(|im| im.net_id_of(bridge_down))
        .unwrap();
    tcs::register_bridge_upstream(
        requester.clone(),
        req_up_read,
        req_up_write,
        requester_up_queue,
        None,
        None,
    )
    .await
    .unwrap();

    let _ = timeout(
        Duration::from_millis(500),
        bridge.endpoints().request::<ErgotPingEndpoint>(
            Address {
                network_id: bridge_down_net,
                node_id: 2,
                port_id: 0,
            },
            &0u32,
            Some("ping"),
        ),
    )
    .await;
    assert_eq!(
        wait_interface_active(&requester, UPSTREAM_IDENT).await,
        bridge_down_net
    );

    (root, bridge, requester)
}

#[tokio::test]
async fn lost_upstream_assignment_response_does_not_block_later_requests() {
    let (root, bridge, requester) = setup_delegation_chain().await;

    let bridge_handler = tokio::spawn({
        let stack = bridge.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let blackhole = tokio::spawn({
        let stack = root.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<ErgotSeedRouterAssignmentEndpoint, 4>(None);
            tokio::pin!(server);
            let mut handle = server.attach();
            ready_tx.send(()).unwrap();
            loop {
                if handle.recv_manual().await.is_ok() {
                    break;
                }
            }
            received_tx.send(()).unwrap();
            core::future::pending::<()>().await;
        }
    });
    ready_rx.await.unwrap();

    let first_request = tokio::spawn({
        let stack = requester.clone();
        async move { request_seed_lease(&stack, UPSTREAM_IDENT).await }
    });
    timeout(Duration::from_secs(1), received_rx)
        .await
        .expect("the blackhole root never received the delegated request")
        .unwrap();

    blackhole.abort();
    let _ = blackhole.await;
    let root_handler = tokio::spawn({
        let stack = root.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });
    tokio::task::yield_now().await;

    let first_err = timeout(Duration::from_secs(2), first_request)
        .await
        .expect("the unanswered upstream RPC did not time out")
        .expect("the first requester task panicked")
        .expect_err("the blackholed assignment unexpectedly succeeded");
    assert!(matches!(
        first_err,
        SeedClientError::AssignmentDenied(SeedAssignmentError::UpstreamUnavailable)
    ));

    let second = timeout(
        Duration::from_secs(1),
        request_seed_lease(&requester, UPSTREAM_IDENT),
    )
    .await
    .expect("one lost upstream response blocked the seed handler permanently")
    .expect("the healthy root should grant the later request");
    assert_ne!(second.net_id, 0);

    bridge_handler.abort();
    root_handler.abort();
}

#[tokio::test]
async fn disappearing_downlink_releases_the_upstream_assignment() {
    let (root, bridge, requester) = setup_delegation_chain().await;
    let bridge_handler = tokio::spawn({
        let stack = bridge.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (assigned_tx, assigned_rx) = tokio::sync::oneshot::channel();
    let (released_tx, released_rx) = tokio::sync::oneshot::channel();
    let root_handler = tokio::spawn({
        let stack = root.clone();
        async move {
            let assignment = stack
                .endpoints()
                .bounded_server::<ErgotSeedRouterAssignmentEndpoint, 4>(None);
            let release = stack
                .endpoints()
                .bounded_server::<ErgotSeedRouterReleaseEndpoint, 4>(None);
            tokio::pin!(assignment);
            tokio::pin!(release);
            let mut assignment = assignment.attach();
            let mut release = release.attach();
            let release_port = release.port();
            ready_tx.send(()).unwrap();
            let mut assigned_tx = Some(assigned_tx);
            let mut released_tx = Some(released_tx);
            loop {
                match embassy_futures::select::select(
                    assignment.recv_manual(),
                    release.recv_manual(),
                )
                .await
                {
                    embassy_futures::select::Either::First(Ok(req)) => {
                        let result = stack
                            .manage_profile(|p| p.request_seed_net_assign(req.hdr.src.network_id))
                            .map(|assignment| SeedRouterAssignment {
                                assignment,
                                refresh_port: 0,
                                release_port,
                            });
                        if let Some(tx) = assigned_tx.take() {
                            tx.send(()).unwrap();
                        }
                        sleep(Duration::from_millis(100)).await;
                        stack
                            .endpoints()
                            .respond_owned::<ErgotSeedRouterAssignmentEndpoint>(&req.hdr, &result)
                            .unwrap();
                    }
                    embassy_futures::select::Either::Second(Ok(req)) => {
                        let result = stack.manage_profile(|p| {
                            p.release_seed_net_assignment(
                                req.hdr.src.network_id,
                                req.t.release_net,
                                req.t.refresh_token,
                            )
                        });
                        stack
                            .endpoints()
                            .respond_owned::<ErgotSeedRouterReleaseEndpoint>(&req.hdr, &result)
                            .unwrap();
                        if let Some(tx) = released_tx.take() {
                            tx.send(()).unwrap();
                        }
                    }
                    _ => {}
                }
            }
        }
    });
    ready_rx.await.unwrap();

    let request = tokio::spawn({
        let stack = requester.clone();
        async move { request_seed_lease(&stack, UPSTREAM_IDENT).await }
    });
    timeout(Duration::from_secs(1), assigned_rx)
        .await
        .expect("root did not allocate the delegated lease")
        .unwrap();
    bridge
        .manage_profile(|p| p.deregister_interface(0))
        .unwrap();

    timeout(Duration::from_secs(1), released_rx)
        .await
        .expect("bridge did not roll back the upstream lease")
        .unwrap();
    request.abort();

    let reused = root
        .manage_profile(|p| p.request_seed_net_assign(1))
        .unwrap();
    assert_eq!(reused.net_id, 3, "released root net_id should be reusable");

    bridge_handler.abort();
    root_handler.abort();
}

#[tokio::test]
async fn upstream_seed_error_is_not_reported_as_net_ids_exhausted() {
    let (root, bridge, requester) = setup_delegation_chain().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let root_handler = tokio::spawn({
        let stack = root.clone();
        async move {
            let server = stack
                .endpoints()
                .bounded_server::<ErgotSeedRouterAssignmentEndpoint, 4>(None);
            tokio::pin!(server);
            let mut handle = server.attach();
            ready_tx.send(()).unwrap();
            loop {
                let Ok(req) = handle.recv_manual().await else {
                    continue;
                };
                let response = Err(SeedAssignmentError::ProfileCantSeed);
                stack
                    .endpoints()
                    .respond_owned::<ErgotSeedRouterAssignmentEndpoint>(&req.hdr, &response)
                    .unwrap();
            }
        }
    });
    ready_rx.await.unwrap();

    let bridge_handler = tokio::spawn({
        let stack = bridge.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });
    sleep(Duration::from_millis(20)).await;

    let err = timeout(
        Duration::from_secs(1),
        request_seed_lease(&requester, UPSTREAM_IDENT),
    )
    .await
    .expect("bridge did not relay the upstream seed error")
    .expect_err("assignment should preserve the upstream seed error");

    assert!(matches!(
        err,
        SeedClientError::AssignmentDenied(SeedAssignmentError::ProfileCantSeed)
    ));

    bridge_handler.abort();
    root_handler.abort();
}

#[tokio::test]
async fn disconnected_upstream_refresh_is_not_reported_as_bad_request() {
    let (root, bridge, requester) = setup_delegation_chain().await;
    let root_handler = tokio::spawn({
        let stack = root.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });
    let bridge_handler = tokio::spawn({
        let stack = bridge.clone();
        async move { stack.services().seed_router_request_handler::<4>().await }
    });

    let lease = timeout(
        Duration::from_secs(1),
        request_seed_lease(&requester, UPSTREAM_IDENT),
    )
    .await
    .expect("initial delegated assignment timed out")
    .expect("initial delegated assignment failed");

    bridge
        .manage_profile(|im| im.set_interface_state(UPSTREAM_IDENT, InterfaceState::Down))
        .unwrap();
    let err = timeout(
        Duration::from_secs(1),
        bridge_seed_refresh(&requester, &lease),
    )
    .await
    .expect("bridge did not answer after its upstream disconnected")
    .expect_err("refresh should fail while the upstream is disconnected");

    assert!(matches!(
        err,
        SeedClientError::RefreshDenied(SeedRefreshError::UpstreamUnavailable)
    ));

    bridge_handler.abort();
    root_handler.abort();
}
