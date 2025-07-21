//! "Edge" device profile
//!
//! Edge devices are the simplest device profile, and are intended for simple devices
//! that are on the "edge" of a network, e.g. they have a single upstream connection
//! to a bridge or seed router.
//!
//! These devices use as many tricks as possible to be as simple as possible. They
//! initially start not knowing their network ID, and if a packet is sent to them,
//! they assume the destination net ID is their net ID. They will also blindly send
//! any outgoing packets, rather than trying to determine whether that packet is
//! actually routable to a node on the network.

use log::{debug, trace};
use serde::Serialize;

use crate::{
    Header, ProtocolError,
    interface_manager::{ConstInit, InterfaceManager, InterfaceSendError, InterfaceSink},
    wire_frames::CommonHeader,
};

pub const CENTRAL_NODE_ID: u8 = 1;
pub const EDGE_NODE_ID: u8 = 2;

pub enum SetNetIdError {
    CantSetZero,
    NoActiveSink,
}

pub struct EdgeInterface<S: InterfaceSink> {
    inner: Option<EdgeInterfaceInner<S>>,
}

struct EdgeInterfaceInner<S: InterfaceSink> {
    sink: S,
    net_id: u16,
    seq_no: u16,
}

pub struct CentralInterface<S: InterfaceSink> {
    sink: S,
    net_id: u16,
    seq_no: u16,
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
        EDGE_NODE_ID
    }

    fn other_node_id(&self) -> u8 {
        CENTRAL_NODE_ID
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

// Central

impl<S: InterfaceSink> CentralInterface<S> {
    pub fn new(sink: S, net_id: u16) -> Self {
        Self {
            sink,
            net_id,
            seq_no: 0,
        }
    }

    pub fn net_id(&self) -> u16 {
        assert!(self.net_id != 0);
        self.net_id
    }

    fn own_node_id(&self) -> u8 {
        CENTRAL_NODE_ID
    }

    // fn other_node_id(&self) -> u8 {
    //     EDGE_NODE_ID
    // }

    fn common_send<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<(&'b mut S, CommonHeader), InterfaceSendError> {
        // TODO: Assumption: "we" are always node_id==1
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

impl<S: InterfaceSink> InterfaceManager for CentralInterface<S> {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let (sink, header) = self.common_send(hdr)?;
        let res = sink.send_ty(&header, hdr.any_all.as_ref(), data);

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
        let (sink, header) = self.common_send(hdr)?;
        let res = sink.send_raw(&header, hdr_raw, data);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let (sink, header) = self.common_send(hdr)?;
        let res = sink.send_err(&header, err);

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}
