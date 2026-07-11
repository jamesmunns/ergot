//! "Edge" device profile
//!
//! Edge devices are the second simplest device profile, and are intended for devices
//! that are on the "edge" of a network, e.g. they have a single upstream connection
//! to a bridge or seed router.
//!
//! These devices use as many tricks as possible to be as simple as possible. They
//! initially start not knowing their network ID, and if a packet is sent to them,
//! they assume the destination net ID is their net ID. They will also blindly send
//! any outgoing packets, rather than trying to determine whether that packet is
//! actually routable to a node on the network.

use crate::logging::{debug, warn};

use serde::Serialize;

/// Type alias for a [`DirectEdge`] profile using the embedded-io interface.
#[cfg(any(feature = "embedded-io-async-v0_6", feature = "embedded-io-async-v0_7"))]
pub type EmbeddedIoManager<Q> =
    DirectEdge<crate::interface_manager::interface_impls::embedded_io::IoInterface<Q>>;

/// Type alias for a [`DirectEdge`] profile using the embassy-usb interface.
#[cfg(any(feature = "embassy-usb-v0_5", feature = "embassy-usb-v0_6"))]
pub type EmbassyUsbManager<Q> =
    DirectEdge<crate::interface_manager::interface_impls::embassy_usb::EmbassyInterface<Q>>;

use crate::{
    Header, HeaderSeq, ProtocolError,
    interface_manager::{
        Interface, InterfaceSendError, InterfaceState, Profile, SetStateError, edge_port::EdgePort,
    },
    net_stack::NetStackHandle,
    wire_frames::de_frame,
};

pub use crate::interface_manager::edge_port::{CENTRAL_NODE_ID, EDGE_NODE_ID};

pub enum SetNetIdError {
    CantSetZero,
    NoActiveSink,
}

/// Edge device profile backed by a single `EdgePort`.
pub struct DirectEdge<I: Interface> {
    port: EdgePort<I>,
    /// Closer for signaling workers to stop. Set by `register_*_stream`,
    /// closed when the interface transitions to `Down`.
    #[cfg(feature = "std")]
    closer: Option<std::sync::Arc<maitake_sync::WaitQueue>>,
}

impl<I: Interface> DirectEdge<I> {
    pub const fn new_target(sink: I::Sink) -> Self {
        Self {
            port: EdgePort::new_target(sink),
            #[cfg(feature = "std")]
            closer: None,
        }
    }

    pub const fn new_controller(sink: I::Sink, state: InterfaceState) -> Self {
        Self {
            port: EdgePort::new_controller(sink, state),
            #[cfg(feature = "std")]
            closer: None,
        }
    }

    /// Tear down the interface: stop any running workers and transition to `Down`.
    ///
    /// Call this before re-opening a transport to ensure the old workers
    /// release the transport resource (e.g., serial port).
    #[cfg(feature = "std")]
    pub fn teardown(&mut self) {
        if let Some(closer) = self.closer.take() {
            closer.close();
        }
        let _ = self.port.set_state(InterfaceState::Down);
    }

    /// Store a closer WaitQueue so that workers are signaled when the
    /// interface transitions to `Down` or when a new stream is registered.
    #[cfg(feature = "std")]
    pub fn set_closer(&mut self, closer: std::sync::Arc<maitake_sync::WaitQueue>) {
        // Close any existing workers before replacing
        if let Some(old) = self.closer.take() {
            old.close();
        }
        self.closer = Some(closer);
    }
}

impl<I: Interface> Profile for DirectEdge<I> {
    type InterfaceIdent = ();

    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let mut hdr = hdr.clone();
        hdr.decrement_ttl()?;
        self.port.send(&hdr, data)
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<Self::InterfaceIdent>,
    ) -> Result<(), InterfaceSendError> {
        if source.is_some() {
            return Err(InterfaceSendError::RoutingLoop);
        }
        let mut hdr = hdr.clone();
        hdr.decrement_ttl()?;
        self.port.send_err(&hdr, err)
    }

    fn send_raw(
        &mut self,
        _hdr: &HeaderSeq,
        _data: &[u8],
        _source: Self::InterfaceIdent,
    ) -> Result<(), InterfaceSendError> {
        // As a DirectEdge, we should never accept a raw message, as that must have
        // come from us.
        Err(InterfaceSendError::RoutingLoop)
    }

    fn interface_state(&mut self, _ident: ()) -> Option<InterfaceState> {
        Some(self.port.state())
    }

    fn set_interface_state(
        &mut self,
        _ident: (),
        state: InterfaceState,
    ) -> Result<(), SetStateError> {
        self.port.set_state(state)
    }
}

/// Frame processor for `DirectEdge` profile.
///
/// Discovers `net_id` from incoming frames and transitions the interface to
/// [`InterfaceState::Active`]. Discovery only runs while the *window* is
/// open — after construction or [`reset()`](crate::interface_manager::FrameProcessor::reset)
/// (liveness timeout), including while a sticky ID is provisionally Active — and only from
/// a frame that is plausibly addressed to this device: `dst.node_id` matches
/// our role's node_id AND the profile does not already route
/// `dst.network_id` somewhere else ([`Profile::is_transit_net`]).
///
/// The guard exists for the bridge-upstream use of this processor: transit
/// frames pass through an upstream with the dst nets of *other* segments,
/// and a blind first-frame latch after a liveness reset could adopt a
/// downstream segment's net_id as the upstream's own — the bridge would then
/// emit frames src-addressed as some other device until the next lucky
/// re-discovery. A discovered net is therefore also kept across `reset()` as
/// a sticky hint, so the first frame after a liveness timeout — transit or
/// not — reactivates the interface with the known net, while a frame that
/// passes the guard may still update it (e.g. the root renumbered the
/// segment after a reboot).
pub struct EdgeFrameProcessor {
    net_id: Option<u16>,
    /// Our node_id on this link: [`EDGE_NODE_ID`] in target mode,
    /// [`CENTRAL_NODE_ID`] in controller mode. Used both to filter which
    /// frames may donate a net_id and as the node to (re)activate with.
    own_node: u8,
    /// Closed (`true`) once an Active interface has been observed or
    /// produced; while closed, frames are processed with zero profile
    /// queries. Cleared by `reset()`.
    activated: bool,
    /// `true` after a liveness reset with a sticky net_id. Transit traffic may
    /// reactivate that ID, but the window stays open until an addressed frame
    /// confirms or replaces it.
    rediscovering: bool,
}

impl EdgeFrameProcessor {
    /// Create a new processor with no discovered net_id (target mode).
    pub fn new() -> Self {
        Self {
            net_id: None,
            own_node: EDGE_NODE_ID,
            activated: false,
            rediscovering: false,
        }
    }

    /// Create a new processor with a pre-set net_id (controller mode).
    pub fn new_controller(net_id: u16) -> Self {
        Self {
            net_id: Some(net_id),
            own_node: CENTRAL_NODE_ID,
            activated: false,
            rediscovering: false,
        }
    }
}

impl Default for EdgeFrameProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl<N> crate::interface_manager::FrameProcessor<N> for EdgeFrameProcessor
where
    N: crate::net_stack::NetStackHandle,
{
    fn process_frame(
        &mut self,
        data: &[u8],
        nsh: &N,
        ident: <<N as crate::net_stack::NetStackHandle>::Profile as crate::interface_manager::Profile>::InterfaceIdent,
    ) -> bool {
        process_frame(self, data, nsh, ident)
    }

    fn reset(&mut self) {
        // Keep net_id as a sticky hint: after a liveness timeout the next
        // frame reactivates the interface with the known net even if it is
        // transit, and only a frame passing the discovery guard may replace
        // the net with a different one.
        self.activated = false;
        self.rediscovering = self.net_id.is_some();
    }
}

/// Process one rx worker frame for direct edge workers.
///
/// Returns `true` if the interface state changed (activation or a net_id
/// update) — callers use this to notify state watchers.
pub fn process_frame<N>(
    state: &mut EdgeFrameProcessor,
    data: &[u8],
    nsh: &N,
    ident: <<N as NetStackHandle>::Profile as Profile>::InterfaceIdent,
) -> bool
where
    N: NetStackHandle,
{
    let Some(mut frame) = de_frame(data) else {
        warn!(
            "Decode error! Ignoring frame on net_id {}",
            state.net_id.unwrap_or(0)
        );
        return false;
    };

    debug!("{}: Got Frame!", frame.hdr);

    let mut state_changed = false;

    // Discovery / reactivation window: open after construction or reset()
    // (liveness timeout). In steady state (`activated`) frames are processed
    // with no profile queries at all.
    if !state.activated {
        let dst = frame.hdr.dst;
        let can_donate = dst.network_id != 0 && dst.node_id == state.own_node;
        let discovery = nsh.stack().manage_profile(|im| {
            let if_state = im
                .interface_state(ident.clone())
                .ok_or("Frame for unknown interface")?;
            match if_state {
                // Already Active with a real net (pre-activated by
                // registration code or reassigned by a seed router): adopt it
                // and close a boot-time window.
                InterfaceState::Active { net_id, .. }
                    if net_id != 0 && !state.rediscovering =>
                {
                    state.net_id = Some(net_id);
                    state.activated = true;
                }
                // Not fully confirmed: Inactive/Down, link-local net 0, or a
                // sticky ID provisionally reactivated during rediscovery.
                InterfaceState::Active { .. }
                | InterfaceState::Inactive
                | InterfaceState::Down => {
                    // Only plausible donors pay the transit lookup cost.
                    let donates = can_donate && !im.is_transit_net(dst.network_id);
                    let new_net = if donates {
                        Some(dst.network_id)
                    } else {
                        state.net_id
                    };

                    if let Some(net) = new_net {
                        let already_active = matches!(
                            if_state,
                            InterfaceState::Active { net_id, node_id }
                                if net_id == net && node_id == state.own_node
                        );
                        if !already_active {
                            im.set_interface_state(
                                ident.clone(),
                                InterfaceState::Active {
                                    net_id: net,
                                    node_id: state.own_node,
                                },
                            )
                            .map_err(|_| "Failed to set interface state from frame")?;
                        }
                        state.net_id = Some(net);
                        if donates {
                            state.activated = true;
                            state.rediscovering = false;
                        }
                        state_changed |= !already_active;
                    }
                    // No net known and no donor: keep the window open and
                    // process this frame with link-local addressing.
                }
                // Bus-style local-only state is managed by the claim protocol.
                InterfaceState::ActiveLocal { .. } => state.activated = true,
            }
            Ok::<(), &'static str>(())
        });
        if let Err(_message) = discovery {
            warn!("{}, dropping", _message);
            return state_changed;
        }
    }

    // If the message comes in and has a src net_id of zero,
    // we should rewrite it so it isn't later understood as a
    // local packet.
    //
    // TODO: accept any packet if we don't have a net_id yet?
    if let Some(net) = state.net_id.as_ref()
        && frame.hdr.src.network_id == 0
    {
        if frame.hdr.src.node_id == 0 {
            warn!("Dropping frame with src node_id 0 (stale or local packet received remotely)");
            return state_changed;
        }
        if frame.hdr.src.node_id == state.own_node {
            warn!(
                "Dropping frame with src node_id {} (spoofed as us)",
                state.own_node
            );
            return state_changed;
        }

        frame.hdr.src.network_id = *net;
    }

    // TODO: if the destination IS self.net_id, we could rewrite the
    // dest net_id as zero to avoid a pass through the interface manager.
    //
    // If the dest is 0, should we rewrite the dest as self.net_id? This
    // is the opposite as above, but I dunno how that will work with responses
    let res = match frame.body {
        Ok(body) => nsh.stack().send_raw(&frame.hdr, body, ident),
        Err(e) => {
            // send_err requires a Header instead of a HeaderSeq, so we convert it
            let nshdr: Header = frame.hdr.clone().into();
            nsh.stack().send_err(&nshdr, e, Some(ident))
        }
    };

    #[allow(unused_variables)]
    match res {
        Ok(()) => {}
        Err(e) => {
            // TODO: match on error, potentially try to send NAK?
            warn!("send error: {:?}", e);
        }
    }

    state_changed
}
