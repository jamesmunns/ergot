use crate::{
    Header, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError,
        utils::{
            edge::CentralInterface,
            framed_stream::{self, Interface},
            std::{ReceiverError, StdQueue},
        },
    },
    wire_frames::de_frame,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::{cell::UnsafeCell, mem::MaybeUninit};

use bbq2::prod_cons::framed::FramedConsumer;
use bbq2::traits::storage::BoxedSlice;
use log::{debug, error, info, trace, warn};
use maitake_sync::WaitQueue;
use mutex::ScopedRawMutex;
use nusb::transfer::{Direction, EndpointType, Queue, RequestBuffer, TransferError};
use tokio::select;

pub struct NusbRecvHdl<R: ScopedRawMutex + 'static> {
    stack: &'static NetStack<R, NusbManager>,
    // TODO: when we have more real networking and we could possibly
    // have conflicting net_id assignments, we might need to have a
    // shared ref to an Arc<AtomicU16> or something for net_id?
    //
    // for now, stdtcp assumes it is the only "seed" router, meaning that
    // it is solely in charge of assigning netids
    net_id: u16,
    biq: Queue<RequestBuffer>,
    closer: Arc<WaitQueue>,
    consecutive_errs: usize,
    mtu: u16,
}

pub struct NusbManager {
    init: bool,
    inner: UnsafeCell<MaybeUninit<NusbManagerInner>>,
}

pub struct Node {
    interface: CentralInterface<Interface<StdQueue>>,
}

#[derive(Default)]
pub struct NusbManagerInner {
    // TODO: we probably want something like iddqd for a hashset sorted by
    // net_id, as well as a list of "allocated" netids, mapped to the
    // interface they are associated with
    //
    // TODO: for the no-std version of this, we will need to use the same
    // intrusive list stuff that we use for sockets for holding interfaces.
    interfaces: Vec<Node>,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    OutOfNetIds,
}

// ---- impls ----

impl Node {
    pub fn new(
        net_id: u16,
        outgoing_buffer_size: usize,
        max_ergot_packet_size: u16,
    ) -> (Self, FramedConsumer<StdQueue>) {
        // todo: configurable channel depth
        let q = bbq2::nicknames::Lechon::new_with_storage(BoxedSlice::new(outgoing_buffer_size));
        let ctx = q.framed_producer();
        let crx = q.framed_consumer();

        let ctx = framed_stream::Interface {
            mtu: max_ergot_packet_size,
            prod: ctx,
        };

        let me = Node {
            interface: CentralInterface::new(ctx, net_id),
        };

        (me, crx)
    }
}

// impl NusbRecvHdl
/// How many in-flight requests at once - allows nusb to keep pulling frames
/// even if we haven't processed them host-side yet.
pub(crate) const IN_FLIGHT_REQS: usize = 4;
/// How many consecutive IN errors will we try to recover from before giving up?
pub(crate) const MAX_STALL_RETRIES: usize = 3;

impl<R: ScopedRawMutex + 'static> NusbRecvHdl<R> {
    pub async fn run(mut self) -> Result<(), ReceiverError> {
        let close = self.closer.clone();

        // Wait for the receiver to encounter an error, or wait for
        // the transmitter to signal that it observed an error
        let res = select! {
            run = self.run_inner() => {
                // Halt the TX worker
                self.closer.close();
                Err(run)
            },
            _clf = close.wait() => Err(ReceiverError::SocketClosed),
        };

        // Remove this interface from the list
        self.stack.with_interface_manager(|im| {
            let inner = im.get_or_init_inner();
            inner
                .interfaces
                .retain(|n| n.interface.net_id() != self.net_id);
        });
        res
    }

    pub async fn run_inner(&mut self) -> ReceiverError {
        loop {
            // Rehydrate the queue
            let pending = self.biq.pending();
            for _ in 0..(IN_FLIGHT_REQS.saturating_sub(pending)) {
                self.biq.submit(RequestBuffer::new(self.mtu as usize));
            }

            let res = self.biq.next_complete().await;

            if let Err(e) = res.status {
                self.consecutive_errs += 1;

                error!(
                    "In Worker error: {e:?}, consecutive: {}",
                    self.consecutive_errs
                );

                // Docs only recommend this for Stall, but it seems to work with
                // UNKNOWN on MacOS as well, todo: look into why!
                //
                // Update: This stall condition seems to have been due to an errata in the
                // STM32F4 USB hardware. See https://github.com/embassy-rs/embassy/pull/2823
                //
                // It is now questionable whether we should be doing this stall recovery at all,
                // as it likely indicates an issue with the connected USB device
                let recoverable = match e {
                    TransferError::Stall | TransferError::Unknown => {
                        self.consecutive_errs <= MAX_STALL_RETRIES
                    }
                    TransferError::Cancelled => false,
                    TransferError::Disconnected => false,
                    TransferError::Fault => false,
                };

                let fatal = if recoverable {
                    warn!("Attempting stall recovery!");

                    // Stall recovery shouldn't be used with in-flight requests, so
                    // cancel them all. They'll still pop out of next_complete.
                    self.biq.cancel_all();
                    info!("Cancelled all in-flight requests");

                    // Now we need to join all in flight requests
                    for _ in 0..(IN_FLIGHT_REQS - 1) {
                        let res = self.biq.next_complete().await;
                        info!("Drain state: {:?}", res.status);
                    }

                    // Now we can mark the stall as clear
                    match self.biq.clear_halt() {
                        Ok(()) => false,
                        Err(e) => {
                            error!("Failed to clear stall: {e:?}, Fatal.");
                            true
                        }
                    }
                } else {
                    error!(
                        "Giving up after {} errors in a row, final error: {e:?}",
                        self.consecutive_errs
                    );
                    true
                };

                if fatal {
                    error!("Fatal Error, exiting");
                    // When we close the channel, all pending receivers and subscribers
                    // will be notified
                    return ReceiverError::SocketClosed;
                } else {
                    info!("Potential recovery, resuming NusbWireRx::recv_inner");
                    continue;
                }
            }

            // If we get a good decode, clear the error flag
            if self.consecutive_errs != 0 {
                info!("Clearing consecutive error counter after good header decode");
                self.consecutive_errs = 0;
            }

            trace!("Got message len {}", res.data.len());
            if let Some(mut frame) = de_frame(&res.data) {
                // If the message comes in and has a src net_id of zero,
                // we should rewrite it so it isn't later understood as a
                // local packet.
                if frame.hdr.src.network_id == 0 {
                    assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
                    assert_ne!(frame.hdr.src.node_id, 1, "someone is pretending to be us?");

                    frame.hdr.src.network_id = self.net_id;
                }
                // TODO: if the destination IS self.net_id, we could rewrite the
                // dest net_id as zero to avoid a pass through the interface manager.
                //
                // If the dest is 0, should we rewrite the dest as self.net_id? This
                // is the opposite as above, but I dunno how that will work with responses
                let hdr = frame.hdr.clone();
                let hdr: Header = hdr.into();

                let res = match frame.body {
                    Ok(body) => self.stack.send_raw(&hdr, frame.hdr_raw, body),
                    Err(e) => self.stack.send_err(&hdr, e),
                };
                match res {
                    Ok(()) => {}
                    Err(e) => {
                        // TODO: match on error, potentially try to send NAK?
                        warn!("recv->send error: {e:?}");
                    }
                }
            } else {
                warn!("Decode error! Ignoring frame on net_id {}", self.net_id);
            }
        }
    }
}

// impl NusbManager

impl NusbManager {
    const fn new() -> Self {
        Self {
            init: false,
            inner: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub fn get_nets(&mut self) -> Vec<u16> {
        let inner = self.get_or_init_inner();
        inner
            .interfaces
            .iter()
            .map(|i| i.interface.net_id())
            .collect()
    }

    fn get_or_init_inner(&mut self) -> &mut NusbManagerInner {
        let inner = self.inner.get_mut();
        if self.init {
            unsafe { inner.assume_init_mut() }
        } else {
            let imr = inner.write(NusbManagerInner::default());
            self.init = true;
            imr
        }
    }
}

impl NusbManager {
    fn find<'b>(
        &'b mut self,
        ihdr: &Header,
    ) -> Result<&'b mut CentralInterface<Interface<StdQueue>>, InterfaceSendError> {
        // todo: make this state impossible? enum of dst w/ or w/o key?
        assert!(!(ihdr.dst.port_id == 0 && ihdr.any_all.is_none()));

        let inner = self.get_or_init_inner();

        // todo: dedupe w/ send
        //
        // todo: we only handle direct dests
        let Ok(idx) = inner
            .interfaces
            .binary_search_by_key(&ihdr.dst.network_id, |int| int.interface.net_id())
        else {
            return Err(InterfaceSendError::NoRouteToDest);
        };

        Ok(&mut inner.interfaces[idx].interface)
    }
}

impl InterfaceManager for NusbManager {
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send(hdr, data)
    }

    fn send_raw(
        &mut self,
        hdr: &Header,
        hdr_raw: &[u8],
        data: &[u8],
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send_raw(hdr, hdr_raw, data)
    }

    fn send_err(
        &mut self,
        hdr: &Header,
        err: crate::ProtocolError,
    ) -> Result<(), InterfaceSendError> {
        let intfc = self.find(hdr)?;
        intfc.send_err(hdr, err)
    }
}

impl Default for NusbManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConstInit for NusbManager {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

unsafe impl Sync for NusbManager {}

// impl NusbManagerInner

impl NusbManagerInner {
    pub fn alloc_intfc(
        &mut self,
        max_usb_frame_size: Option<usize>,
        boq: Queue<Vec<u8>>,
        max_ergot_packet_size: u16,
        outgoing_buffer_size: usize,
    ) -> Option<(u16, Arc<WaitQueue>)> {
        let closer = Arc::new(WaitQueue::new());
        if self.interfaces.is_empty() {
            let net_id = 1;
            let (node, crx) = Node::new(net_id, outgoing_buffer_size, max_ergot_packet_size);
            // TODO: We are spawning in a non-async context!
            tokio::task::spawn(tx_worker(
                net_id,
                max_usb_frame_size,
                boq,
                crx,
                closer.clone(),
            ));
            self.interfaces.push(node);
            debug!("Alloc'd net_id 1");
            return Some((net_id, closer));
        } else if self.interfaces.len() >= 65534 {
            warn!("Out of netids!");
            return None;
        }

        let mut net_id = 1;
        // we're not empty, find the lowest free address by counting the
        // indexes, and if we find a discontinuity, allocate the first one.
        for intfc in self.interfaces.iter() {
            if intfc.interface.net_id() > net_id {
                trace!("Found gap: {net_id}");
                break;
            }
            debug_assert!(intfc.interface.net_id() == net_id);
            net_id += 1;
        }
        // EITHER: We've found a gap that we can use, OR we've iterated all
        // interfaces, which means that we had contiguous allocations but we
        // have not exhausted the range.
        debug_assert!(net_id > 0 && net_id != u16::MAX);

        let (node, crx) = Node::new(net_id, outgoing_buffer_size, max_ergot_packet_size);
        debug!("allocated net_id {net_id}");

        tokio::task::spawn(tx_worker(
            net_id,
            max_usb_frame_size,
            boq,
            crx,
            closer.clone(),
        ));
        self.interfaces.push(node);
        self.interfaces
            .sort_unstable_by_key(|i| i.interface.net_id());
        Some((net_id, closer))
    }
}

// Helper functions

async fn tx_worker(
    net_id: u16,
    max_usb_frame_size: Option<usize>,
    boq: Queue<Vec<u8>>,
    rx: FramedConsumer<StdQueue>,
    closer: Arc<WaitQueue>,
) {
    tx_worker_inner(net_id, max_usb_frame_size, boq, rx, &closer).await;
    warn!("Closing interface {net_id}");
    closer.close();
}

async fn tx_worker_inner(
    net_id: u16,
    max_usb_frame_size: Option<usize>,
    mut boq: Queue<Vec<u8>>,
    rx: FramedConsumer<StdQueue>,
    closer: &WaitQueue,
) {
    info!("Started tx_worker for net_id {net_id}");
    loop {
        let rxf = rx.wait_read();
        let clf = closer.wait();

        let frame = select! {
            r = rxf => r,
            _c = clf => {
                return;
            }
        };

        let len = frame.len();
        debug!("sending pkt len:{} on net_id {net_id}", len);

        let needs_zlp = if let Some(mps) = max_usb_frame_size {
            (len % mps) == 0
        } else {
            true
        };

        boq.submit(frame.to_vec());

        // Append ZLP if we are a multiple of max packet
        if needs_zlp {
            boq.submit(vec![]);
        }

        let send_res = boq.next_complete().await;
        if let Err(e) = send_res.status {
            error!("Output Queue Error: {e:?}");
            return;
        }

        if needs_zlp {
            let send_res = boq.next_complete().await;
            if let Err(e) = send_res.status {
                error!("Output Queue Error: {e:?}");
                return;
            }
        }

        frame.release();
    }
}

pub fn register_interface<R: ScopedRawMutex>(
    stack: &'static NetStack<R, NusbManager>,
    device: NewDevice,
    max_ergot_packet_size: u16,
    outgoing_buffer_size: usize,
) -> Result<NusbRecvHdl<R>, Error> {
    stack.with_interface_manager(|im| {
        let inner = im.get_or_init_inner();
        if let Some((addr, closer)) = inner.alloc_intfc(
            device.max_packet_size,
            device.boq,
            max_ergot_packet_size,
            outgoing_buffer_size,
        ) {
            Ok(NusbRecvHdl {
                stack,
                net_id: addr,
                biq: device.biq,
                closer,
                consecutive_errs: 0,
                mtu: max_ergot_packet_size,
            })
        } else {
            Err(Error::OutOfNetIds)
        }
    })
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct DeviceInfo {
    usb_serial_number: Option<String>,
    usb_manufacturer: Option<String>,
    usb_product: Option<String>,
}

pub struct NewDevice {
    pub info: DeviceInfo,
    pub biq: Queue<RequestBuffer>,
    pub boq: Queue<Vec<u8>>,
    pub max_packet_size: Option<usize>,
}

fn device_match(d1: &nusb::DeviceInfo, d2: &nusb::DeviceInfo) -> bool {
    let bus_match = d1.bus_number() == d2.bus_number();
    let addr_match = d1.device_address() == d2.device_address();
    #[cfg(target_os = "macos")]
    let registry_match = d1.registry_entry_id() == d2.registry_entry_id();
    #[cfg(not(target_os = "macos"))]
    let registry_match = true;

    bus_match && addr_match && registry_match
}

pub async fn find_new_devices(devs: &HashSet<DeviceInfo>) -> Vec<NewDevice> {
    trace!("Searching for new devices...");
    let mut out = vec![];
    let devices = nusb::list_devices().unwrap();
    let devices = devices.filter(coarse_device_filter).collect::<Vec<_>>();

    for device in devices {
        let dinfo = DeviceInfo {
            usb_serial_number: device.serial_number().map(String::from),
            usb_manufacturer: device.manufacturer_string().map(String::from),
            usb_product: device.product_string().map(String::from),
        };
        if devs.contains(&dinfo) {
            continue;
        };

        let mut devices = match nusb::list_devices() {
            Ok(d) => d,
            Err(e) => {
                warn!("Error listing devices: {e:?}");
                return vec![];
            }
        };
        let Some(found) = devices.find(|d| device_match(d, &device)) else {
            warn!("Failed to find matching nusb device!");
            continue;
        };

        // NOTE: We can't enumerate interfaces on Windows. For now, just use
        // a hardcoded interface of zero instead of trying to find the right one
        #[cfg(not(target_os = "windows"))]
        let Some(interface_id) = found.interfaces().position(|i| i.class() == 0xFF) else {
            warn!("Failed to find matching interface!!");
            continue;
        };

        #[cfg(target_os = "windows")]
        let interface_id = 0;

        let dev = match found.open() {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed opening device: {e:?}");
                continue;
            }
        };
        let interface = match dev.claim_interface(interface_id as u8) {
            Ok(i) => i,
            Err(e) => {
                warn!("Failed claiming interface: {e:?}");
                continue;
            }
        };

        let mut mps: Option<usize> = None;
        let mut ep_in: Option<u8> = None;
        let mut ep_out: Option<u8> = None;
        for ias in interface.descriptors() {
            for ep in ias
                .endpoints()
                .filter(|e| e.transfer_type() == EndpointType::Bulk)
            {
                match ep.direction() {
                    Direction::Out => {
                        mps = Some(match mps.take() {
                            Some(old) => old.min(ep.max_packet_size()),
                            None => ep.max_packet_size(),
                        });
                        ep_out = Some(ep.address());
                    }
                    Direction::In => ep_in = Some(ep.address()),
                }
            }
        }

        if let Some(max_packet_size) = &mps {
            debug!("Detected max packet size: {max_packet_size}");
        } else {
            warn!("Unable to detect Max Packet Size!");
        };

        let Some(ep_out) = ep_out else {
            warn!("Failed to find OUT EP");
            continue;
        };
        debug!("OUT EP: {ep_out}");

        let Some(ep_in) = ep_in else {
            warn!("Failed to find IN EP");
            continue;
        };
        debug!("IN EP: {ep_in}");

        let boq = interface.bulk_out_queue(ep_out);
        let biq = interface.bulk_in_queue(ep_in);

        out.push(NewDevice {
            info: dinfo,
            biq,
            boq,
            max_packet_size: mps,
        });
    }

    if !out.is_empty() {
        info!("Found {} new devices", out.len());
    }
    out
}

fn coarse_device_filter(info: &nusb::DeviceInfo) -> bool {
    info.interfaces().any(|intfc| {
        let pre_check =
            intfc.class() == 0xFF && intfc.subclass() == 0xCA && intfc.protocol() == 0x7D;

        pre_check
            && intfc
                .interface_string()
                .map(|s| s == "ergot")
                .unwrap_or(true)
    })
}
