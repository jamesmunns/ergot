// What makes up an interface?
//
// It's generally currently modeled as a channel on the way out, because sends need to be
// immediate from the netstack's perspective, but take time on an interface. On the way
// in, it's sort of "dealer's choice", usually the interface buffers up a frame, immediately
// sends it to the net stack, then continues. We could do double buffering if we need, but
// it's not required.
//
// From the net stack's perspective, the interface manager has some responsibilities:
//
// 1. Contain all of the interfaces (0..N)
// 2. Routing of outgoing messages
// 3. Feeding incoming messages back into the stack
//
// I'd really like to have two "dimensions" for interfaces/interface managers:
//
// * The "role" of the system, e.g. "edge", "bridge", "seed router"
// * The "wire wrangling" of the interface
//
// Right now all interfaces also are one of `cobs_stream` or `framed_stream`, but this is sort
// of a half abstraction.
//
// This also doesn't entirely address the full behavior of an interface: the rx worker side
// of interfaces are more or less detached, though SOME synchronization is required because both
// halves need to know things like the net_id of the interface, especially when it's possible
// that the interface manager may assign the net_id based on info from the seed router.
//
// An interface slot needs:
//
// * A hole to put an InterfaceSink
// * A hole to put an InterfaceSource notifier
// * A hole to put metadata like net id
//
// Okay idea:
//
// * Interfaces define their own bespoke type.
// * Interfaces have some kind of opaque type erasure handle thing that has necessary methds
// * You add interfaces to the interface manager by giving it two things:
//   * A name hash
//   * The opaque handle, which holds:
//     * A vtable/vtable ref
//     * A type ID of the interface
//     * A raw ptr to the interface
// * you can access an interface by doing the bag of holding trick:
//
// ```rust
// impl NetStack {
//     fn with_interface::<T, F, R>(nash: NameHash, f: F) -> Result<R, Error>
//     where
//         F: FnOnce(&mut T) -> R,
//     {
//         // ...
//     }
// }
// ```
//
// That way, interfaces only need to know how to get to the net stack.
//
// New day, thinking about roles and lifecycle:
//
// * Sink
//     * Needs to listen to state changes
//         * Does it? Or should the netstack handle this?
//     * It needs to just be a hole to put things in
//     * I could mandate this is some kind of bbq2 prodicer, or
// * Source
//     * Not an actual thing?
//     * Really just needs a handle to modify and read state?
//     * Interfaces currently just feed
// * State
//     * Link Down
//     * Link Up
//         * Link Inactive (no net)
//         * Link Active (yes net)

use log::{debug, trace};
use serde::Serialize;

use crate::{
    Header, ProtocolError,
    interface_manager::{ConstInit, InterfaceManager, InterfaceSendError, InterfaceSink},
    wire_frames::CommonHeader,
};

pub struct EdgeInterface<S: InterfaceSink> {
    inner: Option<EdgeInterfaceInner<S>>,
}

pub enum SetNetIdError {
    CantSetZero,
    NoActiveSink,
}

impl<S: InterfaceSink> EdgeInterface<S> {
    pub const fn new() -> Self {
        Self { inner: None }
    }

    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    pub fn deregister(&mut self) -> Option<S> {
        self.inner.take().map(|i| i.sink)
    }

    pub fn net_id(&self) -> Option<u16> {
        let i = self.inner.as_ref()?;
        if i.net_id != 0 { Some(i.net_id) } else { None }
    }

    pub fn set_net_id(&mut self, id: u16) -> Result<(), SetNetIdError> {
        if id == 0 {
            return Err(SetNetIdError::CantSetZero);
        }
        let Some(i) = self.inner.as_mut() else {
            return Err(SetNetIdError::NoActiveSink);
        };
        i.net_id = id;
        Ok(())
    }

    pub fn register(&mut self, sink: S) {
        self.inner.replace(EdgeInterfaceInner {
            sink,
            net_id: 0,
            seq_no: 0,
        });
    }
}

impl<S: InterfaceSink> Default for EdgeInterface<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: InterfaceSink> ConstInit for EdgeInterface<S> {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

struct EdgeInterfaceInner<S: InterfaceSink> {
    sink: S,
    net_id: u16,
    seq_no: u16,
}

impl<S: InterfaceSink> EdgeInterface<S> {
    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut S, CommonHeader), InterfaceSendError> {
        let Some(inner) = self.inner.as_mut() else {
            return Err(InterfaceSendError::NoRouteToDest);
        };
        inner.common_send(ihdr)
    }
}

impl<S: InterfaceSink> EdgeInterfaceInner<S> {
    fn own_node_id(&self) -> u8 {
        2
    }

    fn other_node_id(&self) -> u8 {
        1
    }

    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut S, CommonHeader), InterfaceSendError> {
        trace!("common_send header: {:?}", ihdr);

        if self.net_id == 0 {
            debug!("Attempted to send via interface before we have been assigned a net ID");
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        if ihdr.dst.network_id == self.net_id && ihdr.dst.node_id == self.own_node_id() {
            return Err(InterfaceSendError::DestinationLocal);
        }

        // Now that we've filtered out "dest local" checks, see if there is
        // any TTL left before we send to the next hop
        let mut hdr = ihdr.clone();
        hdr.decrement_ttl()?;

        // If the source is local, rewrite the source using this interface's
        // information so responses can find their way back here
        if hdr.src.net_node_any() {
            // todo: if we know the destination is EXACTLY this network,
            // we could leave the network_id local to allow for shorter
            // addresses
            hdr.src.network_id = self.net_id;
            hdr.src.node_id = self.own_node_id();
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = self.net_id;
            hdr.dst.node_id = self.other_node_id();
        }

        let seq_no = self.seq_no;
        self.seq_no = self.seq_no.wrapping_add(1);

        let header = CommonHeader {
            src: hdr.src,
            dst: hdr.dst,
            seq_no,
            kind: hdr.kind,
            ttl: hdr.ttl,
        };
        if [0, 255].contains(&hdr.dst.port_id) && ihdr.any_all.is_none() {
            return Err(InterfaceSendError::AnyPortMissingKey);
        }

        Ok((&mut self.sink, header))
    }
}

impl<S: InterfaceSink> InterfaceManager for EdgeInterface<S> {
    fn send<T: Serialize>(&mut self, hdr: &Header, data: &T) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_ty(&header, hdr.any_all.as_ref(), data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(&mut self, hdr: &Header, err: ProtocolError) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let (intfc, header) = self.common_send(hdr)?;

        let res = intfc.send_raw(&header, hdr_raw, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}
