//! Generic tokio UDP RxWorker and TxWorker.
//!
//! Works with any [`FrameProcessor`] and a shared [`UdpSocket`].
//! UDP datagrams are treated as complete frames (no COBS encoding).
//!
//! [`FrameProcessor`]: crate::interface_manager::FrameProcessor

use std::net::SocketAddr;
use std::sync::Arc;

use crate::{
    interface_manager::{
        FrameProcessor, InterfaceState, LivenessConfig, Profile,
        utils::std::{ReceiverError, StdQueue},
    },
    logging::{error, info, trace, warn},
    net_stack::NetStackHandle,
};
use bbqueue::prod_cons::framed::FramedConsumer;
use maitake_sync::WaitQueue;
use tokio::{net::UdpSocket, select, sync::watch};

/// A generic UDP datagram RxWorker for tokio-based transports.
///
/// Each received UDP datagram is treated as a complete frame and
/// passed to the [`FrameProcessor`].
///
/// On liveness timeout, transitions to [`InterfaceState::Down`]
/// (not Inactive) because UDP is connectionless — there is no
/// persistent connection to recover.
///
/// The caller is responsible for cleanup after [`run`](Self::run)
/// returns.
pub struct UdpRxWorker<N, P>
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    pub nsh: N,
    pub skt: Arc<UdpSocket>,
    pub closer: Arc<WaitQueue>,
    pub processor: P,
    pub ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
    pub liveness: Option<LivenessConfig>,
    pub state_notify: Option<Arc<WaitQueue>>,
}

/// Result of receiving a UDP datagram.
pub struct RecvResult {
    /// Number of bytes received.
    pub len: usize,
    /// Source address of the datagram.
    pub addr: SocketAddr,
}

impl<N, P> UdpRxWorker<N, P>
where
    N: NetStackHandle,
    P: FrameProcessor<N>,
{
    fn notify(&self) {
        if let Some(notify) = &self.state_notify {
            notify.wake_all();
        }
    }

    /// Run the receive loop.
    ///
    /// Returns each received datagram's source address via the callback
    /// `on_recv`, allowing callers to implement peer discovery.
    /// Returns `ReceiverError` when the connection is lost.
    pub async fn run(&mut self, mut on_recv: impl FnMut(SocketAddr)) -> ReceiverError {
        let mut raw_buf = vec![0u8; 4096].into_boxed_slice();
        let mut have_received = false;

        loop {
            let rd = self.skt.recv_from(&mut raw_buf);
            let close = self.closer.wait();

            let liveness_active = self.liveness.is_some() && have_received;

            let (ct, remote_addr) = if liveness_active {
                let liveness = self.liveness.as_ref().unwrap();
                let timeout =
                    tokio::time::sleep(tokio::time::Duration::from_millis(liveness.timeout_ms));

                select! {
                    r = rd => {
                        match r {
                            Ok((0, _)) => {
                                warn!("received nothing, retrying");
                                continue;
                            }
                            Err(e) => {
                                warn!("receiver error, retrying. error: {}, kind: {}", e, e.kind());
                                continue;
                            }
                            Ok((ct, addr)) => {
                                trace!("received {} bytes from {}", ct, addr);
                                (ct, addr)
                            }
                        }
                    }
                    _c = close => {
                        return ReceiverError::SocketClosed;
                    }
                    _ = timeout => {
                        warn!("Liveness timeout — interface down");
                        self.nsh.stack().manage_profile(|im| {
                            _ = im.set_interface_state(
                                self.ident.clone(),
                                InterfaceState::Down,
                            );
                        });
                        self.notify();
                        return ReceiverError::SocketClosed;
                    }
                }
            } else {
                select! {
                    r = rd => {
                        match r {
                            Ok((0, _)) => {
                                warn!("received nothing, retrying");
                                continue;
                            }
                            Err(e) => {
                                warn!("receiver error, retrying. error: {}, kind: {}", e, e.kind());
                                continue;
                            }
                            Ok((ct, addr)) => {
                                trace!("received {} bytes from {}", ct, addr);
                                (ct, addr)
                            }
                        }
                    }
                    _c = close => {
                        return ReceiverError::SocketClosed;
                    }
                }
            };

            have_received = true;
            on_recv(remote_addr);

            let buf = &mut raw_buf[..ct];
            let changed = self
                .processor
                .process_frame(buf, &self.nsh, self.ident.clone());
            if changed {
                self.notify();
            }
        }
    }
}

/// A generic UDP TxWorker for tokio-based transports.
///
/// Reads serialized frames from a [`FramedConsumer`] and sends them
/// via a shared [`UdpSocket`].
///
/// Supports optional peer discovery: if `peer_rx` is `Some`, the
/// worker waits for a peer address before sending (used by
/// unconnected target sockets). If `peer_rx` is `None`, uses
/// `socket.send()` (for connected controller sockets).
///
/// On exit, calls `closer.close()` to ensure the RxWorker also
/// shuts down.
pub struct UdpTxWorker {
    pub socket: Arc<UdpSocket>,
    pub consumer: FramedConsumer<StdQueue>,
    pub closer: Arc<WaitQueue>,
    pub peer_rx: Option<watch::Receiver<Option<SocketAddr>>>,
}

impl UdpTxWorker {
    pub async fn run(mut self) {
        info!("Started UDP tx_worker");

        // For unconnected sockets (target), wait for peer address from RxWorker
        let peer_addr = if let Some(ref mut peer_rx) = self.peer_rx {
            loop {
                let clf = self.closer.wait();
                let changed = peer_rx.changed();

                select! {
                    r = changed => {
                        if r.is_err() {
                            warn!("Peer address channel closed");
                            self.closer.close();
                            return;
                        }
                        if let Some(addr) = *peer_rx.borrow() {
                            info!("Learned peer address: {}", addr);
                            break Some(addr);
                        }
                    }
                    _c = clf => {
                        return;
                    }
                }
            }
        } else {
            None // Connected socket (controller) — use send()
        };

        loop {
            let rxf = self.consumer.wait_read();
            let clf = self.closer.wait();

            let frame = select! {
                r = rxf => r,
                _c = clf => {
                    break;
                }
            };

            let len = frame.len();
            trace!("sending UDP pkt len:{}", len);
            let res = match peer_addr {
                Some(addr) => self.socket.send_to(&frame, addr).await,
                None => self.socket.send(&frame).await,
            };
            frame.release();
            if let Err(e) = res {
                error!("Tx Error: {:?}", e);
                break;
            }
        }
        warn!("Closing UDP tx_worker");
        self.closer.close();
    }
}

// ---------------------------------------------------------------------------
// Registration: DirectEdge
// ---------------------------------------------------------------------------

use crate::interface_manager::Interface;
use crate::interface_manager::profiles::direct_edge::{DirectEdge, EdgeFrameProcessor};
use bbqueue::traits::bbqhdl::BbqHandle;

/// Registration error for DirectEdge.
#[derive(Debug, PartialEq)]
pub struct EdgeRegistrationError;

/// Register a UDP transport on a [`DirectEdge`] profile.
///
/// `initial_state` controls target vs controller mode:
/// - Target: `InterfaceState::Inactive` with `EdgeFrameProcessor::new()`
///   — creates a watch channel internally for peer address learning.
/// - Controller: `InterfaceState::Active { net_id: 1, node_id: 1 }` with
///   `EdgeFrameProcessor::new_controller(1)` — uses connected socket (`send()`).
pub async fn register_edge<N, I>(
    stack: N,
    socket: UdpSocket,
    queue: StdQueue,
    processor: EdgeFrameProcessor,
    initial_state: InterfaceState,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<(), EdgeRegistrationError>
where
    I: Interface,
    N: NetStackHandle<Profile = DirectEdge<I>> + Send + 'static,
{
    let arc_socket = Arc::new(socket);
    let closer = Arc::new(WaitQueue::new());

    // Determine if target mode (Inactive) for peer discovery
    let is_target = matches!(initial_state, InterfaceState::Inactive);

    stack.stack().manage_profile(|im| {
        match im.interface_state(()) {
            Some(InterfaceState::Down) | None => {}
            _ => return Err(EdgeRegistrationError),
        }
        im.set_closer(closer.clone());
        im.set_interface_state((), initial_state)
            .map_err(|_| EdgeRegistrationError)?;
        Ok(())
    })?;
    if let Some(notify) = &state_notify {
        notify.wake_all();
    }

    let (peer_sender, peer_receiver) = if is_target {
        let (tx, rx) = watch::channel(None);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let notify_clone = state_notify.clone();
    let stack_clone = stack.clone();

    let mut rx_worker = UdpRxWorker {
        nsh: stack,
        skt: arc_socket.clone(),
        closer: closer.clone(),
        processor,
        ident: (),
        liveness,
        state_notify,
    };

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run(|addr| {
                if let Some(peer_tx) = &peer_sender {
                    let _ = peer_tx.send(Some(addr));
                }
            }) => {
                close.close();
            },
            _clf = close.wait() => {},
        }
        stack_clone.stack().manage_profile(|im| {
            _ = im.set_interface_state((), InterfaceState::Down);
        });
        if let Some(notify) = &notify_clone {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        UdpTxWorker {
            socket: arc_socket,
            consumer: queue.framed_consumer(),
            closer: closer.clone(),
            peer_rx: peer_receiver,
        }
        .run(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration: Router
// ---------------------------------------------------------------------------

use crate::interface_manager::profiles::router::{Router, RouterFrameProcessor};
use crate::interface_manager::utils::framed_stream::Sink;
use crate::interface_manager::utils::std::new_std_queue;
use rand_core::RngCore;

/// Registration error for Router.
#[derive(Debug, PartialEq)]
pub struct RouterRegistrationError;

/// Register a UDP transport on a [`Router`] profile.
///
/// Router always uses connected sockets, so no peer discovery is needed.
pub async fn register_router<N, I, Rng, const M: usize, const SS: usize>(
    stack: N,
    socket: UdpSocket,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
    liveness: Option<LivenessConfig>,
    state_notify: Option<Arc<WaitQueue>>,
) -> Result<u8, RouterRegistrationError>
where
    I: Interface<Sink = Sink<StdQueue>>,
    Rng: RngCore + Send + 'static,
    N: NetStackHandle<Profile = Router<I, Rng, M, SS>> + Send + 'static,
{
    let arc_socket = Arc::new(socket);
    let q: StdQueue = new_std_queue(outgoing_buffer_size);
    let res = stack.stack().manage_profile(|im| {
        let ident = im
            .register_interface(Sink::new_from_handle(q.clone(), max_ergot_packet_size))
            .ok()?;
        let state = im.interface_state(ident)?;
        match state {
            InterfaceState::Active { net_id, node_id: _ } => Some((ident, net_id)),
            _ => {
                _ = im.deregister_interface(ident);
                None
            }
        }
    });
    let Some((ident, net_id)) = res else {
        return Err(RouterRegistrationError);
    };
    let closer = Arc::new(WaitQueue::new());

    let notify_clone = state_notify.clone();
    let nsh_clone = stack.clone();

    let mut rx_worker = UdpRxWorker {
        nsh: stack.clone(),
        skt: arc_socket.clone(),
        closer: closer.clone(),
        processor: RouterFrameProcessor::new(net_id),
        ident,
        liveness,
        state_notify,
    };

    stack.stack().manage_profile(|im| {
        im.set_interface_closer(ident, closer.clone());
    });

    tokio::task::spawn(async move {
        let close = rx_worker.closer.clone();
        select! {
            _run = rx_worker.run(|_addr| {}) => {
                close.close();
            },
            _clf = close.wait() => {},
        }
        nsh_clone.stack().manage_profile(|im| {
            _ = im.deregister_interface(ident);
        });
        if let Some(notify) = &notify_clone {
            notify.wake_all();
        }
    });
    tokio::task::spawn(
        UdpTxWorker {
            socket: arc_socket,
            consumer: <StdQueue as BbqHandle>::framed_consumer(&q),
            closer: closer.clone(),
            peer_rx: None,
        }
        .run(),
    );

    Ok(ident)
}
