use core::{any::TypeId, ptr::NonNull};

use cordyceps::List;
use log::{debug, trace};
use serde::Serialize;

use crate::{
    FrameKind, Header, ProtocolError,
    interface_manager::{self, InterfaceSendError, Profile},
    net_stack::NetStackSendError,
    socket::{SocketHeader, SocketSendError, SocketVTable, borser},
};

pub(crate) struct NetStackInner<P: Profile> {
    pub(super) sockets: List<SocketHeader>,
    pub(super) profile: P,
    pub(super) pcache_bits: u32,
    pub(super) pcache_start: u8,
    pub(super) seq_no: u16,
}

// ---- impl NetStackInner ----

impl<P> NetStackInner<P>
where
    P: Profile,
    P: interface_manager::ConstInit,
{
    pub const fn new() -> Self {
        Self {
            sockets: List::new(),
            profile: P::INIT,
            seq_no: 0,
            pcache_bits: 0,
            pcache_start: 0,
        }
    }
}

impl<P> NetStackInner<P>
where
    P: Profile,
{
    /// Create a netstack with a given profile
    pub const fn new_with_profile(p: P) -> Self {
        Self {
            sockets: List::new(),
            profile: p,
            seq_no: 0,
            pcache_bits: 0,
            pcache_start: 0,
        }
    }

    /// Method that handles broadcast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn broadcast<SendSockets, SendProfile>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        mut sskt: SendSockets,
        smgr: SendProfile,
    ) -> Result<(), NetStackSendError>
    where
        SendSockets: FnMut(NonNull<SocketHeader>) -> bool,
        SendProfile: FnOnce() -> bool,
    {
        trace!("Sending msg broadcast w/ header: {hdr:?}");
        let res_lcl = {
            let bcast_iter = Self::find_all_local(sockets, hdr)?;
            let mut any_found = false;
            for dst in bcast_iter {
                let res = sskt(dst);
                if res {
                    debug!("delivered broadcast message locally");
                }
                any_found |= res;
            }
            any_found
        };

        let res_rmt = smgr();
        if res_rmt {
            debug!("delivered broadcast message remotely");
        }

        if res_lcl || res_rmt {
            Ok(())
        } else {
            Err(NetStackSendError::NoRoute)
        }
    }

    /// Method that handles unicast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn unicast<SendSockets, SendProfile>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        sskt: SendSockets,
        smgr: SendProfile,
    ) -> Result<(), NetStackSendError>
    where
        SendSockets: FnOnce(NonNull<SocketHeader>) -> Result<(), NetStackSendError>,
        SendProfile: FnOnce() -> Result<(), InterfaceSendError>,
    {
        trace!("Sending msg unicast w/ header: {hdr:?}");
        // Can we assume the destination is local?
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        let res = if !local_bypass {
            // Not local: offer to the interface manager to send
            debug!("Offering msg externally unicast w/ header: {hdr:?}");
            smgr()
        } else {
            // just skip to local sending
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => {
                debug!("Externally routed msg unicast");
                return Ok(());
            }
            Err(InterfaceSendError::DestinationLocal) => {
                debug!("No external interest in msg unicast");
            }
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        let socket = if hdr.dst.port_id == 0 {
            debug!("Sending ANY unicast msg locally w/ header: {hdr:?}");
            Self::find_any_local(sockets, hdr)
        } else {
            debug!("Sending ONE unicast msg locally w/ header: {hdr:?}");
            Self::find_one_local(sockets, hdr)
        }?;

        sskt(socket)
    }

    /// Method that handles unicast logic
    ///
    /// Takes closures for sending to a socket or sending to the manager to allow
    /// for abstracting over send_raw/send_ty.
    fn unicast_err<SendSockets, SendProfile>(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
        sskt: SendSockets,
        smgr: SendProfile,
    ) -> Result<(), NetStackSendError>
    where
        SendSockets: FnOnce(NonNull<SocketHeader>) -> Result<(), NetStackSendError>,
        SendProfile: FnOnce() -> Result<(), InterfaceSendError>,
    {
        trace!("Sending err unicast w/ header: {hdr:?}");
        // Can we assume the destination is local?
        let local_bypass = hdr.src.net_node_any() && hdr.dst.net_node_any();

        let res = if !local_bypass {
            // Not local: offer to the interface manager to send
            debug!("Offering err externally unicast w/ header: {hdr:?}");
            smgr()
        } else {
            // just skip to local sending
            Err(InterfaceSendError::DestinationLocal)
        };

        match res {
            Ok(()) => {
                debug!("Externally routed err unicast");
                return Ok(());
            }
            Err(InterfaceSendError::DestinationLocal) => {
                debug!("No external interest in err unicast");
            }
            Err(e) => return Err(NetStackSendError::InterfaceSend(e)),
        }

        // It was a destination local error, try to honor that
        let socket = Self::find_one_err_local(sockets, hdr)?;

        sskt(socket)
    }

    /// Handle sending of a raw (serialized) message
    pub(super) fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        body: &[u8],
        source: P::InterfaceIdent,
    ) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            profile: manager,
            ..
        } = self;
        trace!("Sending msg raw w/ header: {hdr:?} from {source:?}");

        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        if hdr.dst.port_id == 255 {
            Self::broadcast(
                sockets,
                hdr,
                |skt| Self::send_raw_to_socket(skt, body, hdr, hdr_raw, seq_no).is_ok(),
                || manager.send_raw(hdr, hdr_raw, body, source).is_ok(),
            )
        } else {
            Self::unicast(
                sockets,
                hdr,
                |skt| Self::send_raw_to_socket(skt, body, hdr, hdr_raw, seq_no),
                || manager.send_raw(hdr, hdr_raw, body, source),
            )
        }
    }

    /// Handle sending of a typed message
    pub(super) fn send_ty<T: 'static + Serialize + Clone>(
        &mut self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            profile: manager,
            ..
        } = self;
        trace!("Sending msg ty w/ header: {hdr:?}");

        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        if hdr.dst.port_id == 255 {
            Self::broadcast(
                sockets,
                hdr,
                |skt| Self::send_ty_to_socket(skt, t, hdr, seq_no).is_ok(),
                || manager.send(hdr, t).is_ok(),
            )
        } else {
            Self::unicast(
                sockets,
                hdr,
                |skt| Self::send_ty_to_socket(skt, t, hdr, seq_no),
                || manager.send(hdr, t),
            )
        }
    }

    /// Handle sending a borrowed message
    pub(super) fn send_bor<T: Serialize>(
        &mut self,
        hdr: &Header,
        t: &T,
    ) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            profile: manager,
            ..
        } = self;
        trace!("Sending msg bor w/ header: {hdr:?}");

        if hdr.kind == FrameKind::PROTOCOL_ERROR {
            todo!("Don't do that");
        }

        // Is this a broadcast message?
        if hdr.dst.port_id == 255 {
            Self::broadcast(
                sockets,
                hdr,
                |skt| Self::send_bor_to_socket(skt, t, hdr, seq_no).is_ok(),
                || manager.send(hdr, t).is_ok(),
            )
        } else {
            Self::unicast(
                sockets,
                hdr,
                |skt| Self::send_bor_to_socket(skt, t, hdr, seq_no),
                || manager.send(hdr, t),
            )
        }
    }

    /// Handle sending of a typed message
    pub(super) fn send_err(
        &mut self,
        hdr: &Header,
        err: ProtocolError,
        source: Option<P::InterfaceIdent>,
    ) -> Result<(), NetStackSendError> {
        let Self {
            sockets,
            seq_no,
            profile: manager,
            ..
        } = self;
        trace!("Sending msg err w/ header: {hdr:?}");

        if hdr.dst.port_id == 255 {
            todo!("Don't do that");
        }

        Self::unicast_err(
            sockets,
            hdr,
            |skt| Self::send_err_to_socket(skt, err, hdr, seq_no),
            || manager.send_err(hdr, err, source),
        )
    }

    /// Find a specific (e.g. port_id not 0 or 255) destination port matching
    /// the given header.
    fn find_one_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find the specific matching port
        let mut iter = sockets.iter_raw();
        let socket = loop {
            let Some(skt) = iter.next() else {
                return Err(NetStackSendError::NoRoute);
            };
            let skt_ref = unsafe { skt.as_ref() };
            if skt_ref.port != hdr.dst.port_id {
                continue;
            }
            if skt_ref.attrs.kind != hdr.kind {
                return Err(NetStackSendError::WrongPortKind);
            }
            break skt;
        };
        Ok(socket)
    }

    /// Find a specific (e.g. port_id not 0 or 255) destination port matching
    /// the given header.
    fn find_one_err_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find the specific matching port
        let mut iter = sockets.iter_raw();
        let socket = loop {
            let Some(skt) = iter.next() else {
                return Err(NetStackSendError::NoRoute);
            };
            let skt_ref = unsafe { skt.as_ref() };
            if skt_ref.port != hdr.dst.port_id {
                continue;
            }
            break skt;
        };
        Ok(socket)
    }

    /// Find a wildcard (e.g. port_id == 0) destination port matching the given header.
    ///
    /// If more than one port matches the wildcard, an error is returned.
    /// Does not match sockets that does not have the `discoverable` [`Attributes`].
    fn find_any_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<NonNull<SocketHeader>, NetStackSendError> {
        // Find ONE specific matching port
        let Some(apdx) = hdr.any_all.as_ref() else {
            return Err(NetStackSendError::AnyPortMissingKey);
        };
        let mut iter = sockets.iter_raw();
        let mut socket: Option<NonNull<SocketHeader>> = None;

        loop {
            let Some(skt) = iter.next() else {
                break;
            };
            let skt_ref = unsafe { skt.as_ref() };

            // Check for things that would disqualify a socket from being an
            // "ANY" destination
            let mut illegal = false;
            illegal |= skt_ref.attrs.kind != hdr.kind;
            illegal |= !skt_ref.attrs.discoverable;
            illegal |= skt_ref.key != apdx.key;
            if let Some(nash) = apdx.nash {
                illegal |= Some(nash) != skt_ref.nash;
            }

            if illegal {
                // Wait, that's illegal
                continue;
            }

            // It's a match! Is it a second match?
            if socket.is_some() {
                return Err(NetStackSendError::AnyPortNotUnique);
            }
            // Nope! Store this one, then we keep going to ensure that no
            // other socket matches this description.
            socket = Some(skt);
        }

        socket.ok_or(NetStackSendError::NoRoute)
    }

    /// Find ALL broadcast (e.g. port_id == 255) sockets matching the given header.
    ///
    /// Returns an error if the header does not contain a Key. May return zero
    /// matches.
    fn find_all_local(
        sockets: &mut List<SocketHeader>,
        hdr: &Header,
    ) -> Result<impl Iterator<Item = NonNull<SocketHeader>>, NetStackSendError> {
        let Some(any_all) = hdr.any_all.as_ref() else {
            return Err(NetStackSendError::AllPortMissingKey);
        };
        Ok(sockets.iter_raw().filter(move |socket| {
            let skt_ref = unsafe { socket.as_ref() };
            let bport = skt_ref.port == 255;
            let dkind = skt_ref.attrs.kind == hdr.kind;
            let dkey = skt_ref.key == any_all.key;

            // If the any/all message DOES contain a name hash, then ONLY match
            // sockets with the same name hash.
            let name = if let Some(nash) = any_all.nash {
                Some(nash) == skt_ref.nash
            } else {
                true
            };
            bport && dkind && dkey && name
        }))
    }

    /// Helper method for sending a type to a given socket
    fn send_ty_to_socket<T: 'static + Serialize + Clone>(
        this: NonNull<SocketHeader>,
        t: &T,
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };

        if let Some(f) = vtable.recv_owned {
            let this: NonNull<()> = this.cast();
            let that: NonNull<T> = NonNull::from(t);
            let that: NonNull<()> = that.cast();
            let hdr = hdr.to_headerseq_or_with_seq(|| {
                let seq = *seq_no;
                *seq_no = seq_no.wrapping_add(1);
                seq
            });
            (f)(this, that, hdr, &TypeId::of::<T>()).map_err(NetStackSendError::SocketSend)
        } else if let Some(_f) = vtable.recv_bor {
            // TODO: support send borrowed
            todo!()
        } else {
            // todo: keep going? If we found the "right" destination and
            // sending fails, then there's not much we can do. Probably: there
            // is no case where a socket has NEITHER send_owned NOR send_bor,
            // can we make this state impossible instead?
            Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
        }
    }

    /// Helper method for sending a type to a given socket
    fn send_bor_to_socket<T: Serialize>(
        this: NonNull<SocketHeader>,
        t: &T,
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };

        if let Some(f) = vtable.recv_bor {
            let this: NonNull<()> = this.cast();
            let that: NonNull<T> = NonNull::from(t);
            let that: NonNull<()> = that.cast();
            let hdr = hdr.to_headerseq_or_with_seq(|| {
                let seq = *seq_no;
                *seq_no = seq_no.wrapping_add(1);
                seq
            });
            let func = borser::<T>;
            (f)(this, that, hdr, func).map_err(NetStackSendError::SocketSend)
        } else {
            // todo: keep going? If we found the "right" destination and
            // sending fails, then there's not much we can do. Probably: there
            // is no case where a socket has NEITHER send_owned NOR send_bor,
            // can we make this state impossible instead?
            Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
        }
    }

    /// Helper method for sending a type to a given socket
    fn send_err_to_socket(
        this: NonNull<SocketHeader>,
        err: ProtocolError,
        hdr: &Header,
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };

        if let Some(f) = vtable.recv_err {
            let this: NonNull<()> = this.cast();
            let hdr = hdr.to_headerseq_or_with_seq(|| {
                let seq = *seq_no;
                *seq_no = seq_no.wrapping_add(1);
                seq
            });
            (f)(this, hdr, err);
            Ok(())
        } else {
            // todo: keep going? If we found the "right" destination and
            // sending fails, then there's not much we can do. Probably: there
            // is no case where a socket has NEITHER send_owned NOR send_bor,
            // can we make this state impossible instead?
            Err(NetStackSendError::SocketSend(SocketSendError::WhatTheHell))
        }
    }

    /// Helper message for sending a raw message to a given socket
    fn send_raw_to_socket(
        this: NonNull<SocketHeader>,
        body: &[u8],
        hdr: &Header,
        hdr_raw: &[u8],
        seq_no: &mut u16,
    ) -> Result<(), NetStackSendError> {
        let vtable: &'static SocketVTable = {
            let skt_ref = unsafe { this.as_ref() };
            skt_ref.vtable
        };
        let f = vtable.recv_raw;

        let this: NonNull<()> = this.cast();
        let hdr = hdr.to_headerseq_or_with_seq(|| {
            let seq = *seq_no;
            *seq_no = seq_no.wrapping_add(1);
            seq
        });

        (f)(this, body, hdr, hdr_raw).map_err(NetStackSendError::SocketSend)
    }
}

impl<P> NetStackInner<P>
where
    P: Profile,
{
    /// Cache-based allocator inspired by littlefs2 ID allocator
    ///
    /// We remember 32 ports at a time, from the current base, which is always
    /// a multiple of 32. Allocating from this range does not require moving thru
    /// the socket lists.
    ///
    /// If the current 32 ports are all taken, we will start over from a base port
    /// of 0, and attempt to
    pub(super) fn alloc_port(&mut self) -> Option<u8> {
        // ports 0 is always taken (could be clear on first alloc)
        self.pcache_bits |= (self.pcache_start == 0) as u32;

        if self.pcache_bits != u32::MAX {
            // We can allocate from the current slot
            let ldg = self.pcache_bits.trailing_ones();
            debug_assert!(ldg < 32);
            self.pcache_bits |= 1 << ldg;
            return Some(self.pcache_start + (ldg as u8));
        }

        // Nope, cache is all taken. try to find a base with available items.
        // We always start from the bottom to keep ports small, but if we know
        // we just exhausted a range, don't waste time checking that
        let old_start = self.pcache_start;
        for base in 0..8 {
            let start = base * 32;
            if start == old_start {
                continue;
            }
            // Clear/reset cache
            self.pcache_start = start;
            self.pcache_bits = 0;
            // port 0 is not allowed
            self.pcache_bits |= (self.pcache_start == 0) as u32;
            // port 255 is not allowed
            self.pcache_bits |= ((self.pcache_start == 0b111_00000) as u32) << 31;

            // TODO: If we trust that sockets are always sorted, we could early-return
            // when we reach a `pupper > self.pcache_start`. We could also maybe be smart
            // and iterate forwards for 0..4 and backwards for 4..8 (and switch the early
            // return check to < instead). NOTE: We currently do NOT guarantee sockets are
            // sorted!
            self.sockets.iter().for_each(|s| {
                if s.port == 255 {
                    return;
                }

                // The upper 3 bits of the port
                let pupper = s.port & !(32 - 1);
                // The lower 5 bits of the port
                let plower = s.port & (32 - 1);

                if pupper == self.pcache_start {
                    self.pcache_bits |= 1 << plower;
                }
            });

            if self.pcache_bits != u32::MAX {
                // We can allocate from the current slot
                let ldg = self.pcache_bits.trailing_ones();
                debug_assert!(ldg < 32);
                self.pcache_bits |= 1 << ldg;
                return Some(self.pcache_start + (ldg as u8));
            }
        }

        // Nope, nothing found
        None
    }

    pub(super) fn free_port(&mut self, port: u8) {
        debug_assert!(port != 255);
        // The upper 3 bits of the port
        let pupper = port & !(32 - 1);
        // The lower 5 bits of the port
        let plower = port & (32 - 1);

        // TODO: If the freed port is in the 0..32 range, or just less than
        // the current start range, maybe do an opportunistic re-look?
        if pupper == self.pcache_start {
            self.pcache_bits &= !(1 << plower);
        }
    }
}
