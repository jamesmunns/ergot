// I need an interface manager that can have 0 or 1 interfaces
// it needs to be able to be const init'd (empty)
// at runtime we can attach the client (and maybe re-attach?)
//
// In normal setups, we'd probably want some way to "announce" we
// are here, but in point-to-point

use crate::{
    Header, NetStack,
    interface_manager::{
        ConstInit, InterfaceManager, InterfaceSendError,
        framed_stream::{self, Interface},
    },
    wire_frames::{CommonHeader, de_frame},
};
use bbq2::{
    prod_cons::framed::FramedConsumer,
    queue::BBQueue,
    traits::{coordination::Coord, notifier::maitake::MaiNotSpsc, storage::Inline},
};
use core::sync::atomic::{AtomicU8, Ordering};
use defmt::{debug, info, warn};
use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use embassy_usb::{
    Builder, UsbDevice,
    driver::{Driver, Endpoint, EndpointError, EndpointIn, EndpointOut},
    msos::{self, windows_version},
};
use mutex::ScopedRawMutex;
use static_cell::ConstStaticCell;

/// An Embassy USB Bulk Transfer Interface Manager
///
/// Only suitable for cases where this is an "edge" node that will not forward
/// messages onwards. Assumes we only have a single upstream connection.
#[derive(Default)]
pub struct EmbassyUsbManager<const N: usize, C>
where
    C: Coord + 'static,
{
    inner: Option<EmbassyUsbManagerInner<N, C>>,
    seq_no: u16,
}

/// The Receiver wrapper
///
/// This manages the receiver operations, as well as manages the connection state.
///
/// The `N` const generic buffer is the size of the outgoing buffer.
pub struct Receiver<R, D, const N: usize, C>
where
    R: ScopedRawMutex + 'static,
    D: Driver<'static>,
    C: Coord + 'static,
{
    bbq: &'static BBQueue<Inline<N>, C, MaiNotSpsc>,
    stack: &'static NetStack<R, EmbassyUsbManager<N, C>>,
    rx: D::EndpointOut,
    net_id: Option<u16>,
}

/// Inner item visible when we DO have an active connection
struct EmbassyUsbManagerInner<const N: usize, C>
where
    C: Coord + 'static,
{
    interface: ProducerHandle<N, C>,
    net_id: u16,
}

/// Errors observable by the sender
enum TransmitError {
    ConnectionClosed,
    Timeout,
}

/// Errors observable by the receiver
enum ReceiverError {
    ReceivedMessageTooLarge,
    ConnectionClosed,
}

struct ProducerHandle<const N: usize, C>
where
    C: Coord + 'static,
{
    skt_tx: framed_stream::Interface<&'static BBQueue<Inline<N>, C, MaiNotSpsc>>,
}

// ---- impls ----

impl<const N: usize, C> EmbassyUsbManager<N, C>
where
    C: Coord + 'static,
{
    pub const fn new() -> Self {
        Self {
            inner: None,
            seq_no: 0,
        }
    }
}

impl<const N: usize, C> ConstInit for EmbassyUsbManager<N, C>
where
    C: Coord + 'static,
{
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self::new();
}

impl<const N: usize, C> EmbassyUsbManager<N, C>
where
    C: Coord + 'static,
{
    fn common_send<'a, 'b>(
        &'b mut self,
        ihdr: &'a Header,
    ) -> Result<(&'b mut EmbassyUsbManagerInner<N, C>, CommonHeader), InterfaceSendError> {
        let intfc = match self.inner.take() {
            None => {
                warn!("INTFC NONE");
                return Err(InterfaceSendError::NoRouteToDest);
            }
            // TODO: Closed flag?
            // Some(intfc) if intfc.closer.is_closed() => {
            //     drop(intfc);
            //     return Err(InterfaceSendError::NoRouteToDest);
            // }
            Some(intfc) => self.inner.insert(intfc),
        };

        if intfc.net_id == 0 {
            debug!("Attempted to send via interface before we have been assigned a net ID");
            // No net_id yet, don't allow routing (todo: maybe broadcast?)
            return Err(InterfaceSendError::NoRouteToDest);
        }
        // todo: we could probably keep a routing table of some kind, but for
        // now, we treat this as a "default" route, all packets go

        // TODO: a LOT of this is copy/pasted from the router, can we make this
        // shared logic, or handled by the stack somehow?
        //
        // TODO: Assumption: "we" are always node_id==2
        if ihdr.dst.network_id == intfc.net_id && ihdr.dst.node_id == 2 {
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
            hdr.src.network_id = intfc.net_id;
            hdr.src.node_id = 2;
        }

        // If this is a broadcast message, update the destination, ignoring
        // whatever was there before
        if hdr.dst.port_id == 255 {
            hdr.dst.network_id = intfc.net_id;
            hdr.dst.node_id = 1;
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
        if [0, 255].contains(&hdr.dst.port_id) {
            if ihdr.any_all.is_none() {
                return Err(InterfaceSendError::AnyPortMissingKey);
            }
        }

        Ok((intfc, header))
    }
}

impl<const N: usize, C> InterfaceManager for EmbassyUsbManager<N, C>
where
    C: Coord + 'static,
{
    fn send<T: serde::Serialize>(
        &mut self,
        hdr: &Header,
        data: &T,
    ) -> Result<(), InterfaceSendError> {
        debug!("eum::send");
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc
            .interface
            .skt_tx
            .send_ty(&header, hdr.any_all.as_ref(), data);
        debug!("eum::send done: {=bool}", res.is_ok());

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
        debug!("eum::send_raw");
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_raw(&header, hdr_raw, data);
        debug!("eum::send_raw done: {=bool}", res.is_ok());

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
        debug!("eum::send_err");
        let (intfc, header) = self.common_send(hdr)?;
        let res = intfc.interface.skt_tx.send_err(&header, err);
        debug!("eum::send_err done: {=bool}", res.is_ok());

        match res {
            Ok(()) => Ok(()),
            Err(()) => Err(InterfaceSendError::InterfaceFull),
        }
    }
}

impl<R, D, const N: usize, C> Receiver<R, D, N, C>
where
    R: ScopedRawMutex + 'static,
    D: Driver<'static>,
    C: Coord,
{
    /// Create a new receiver object
    pub fn new(
        q: &'static BBQueue<Inline<N>, C, MaiNotSpsc>,
        stack: &'static NetStack<R, EmbassyUsbManager<N, C>>,
        rx: D::EndpointOut,
    ) -> Self {
        Self {
            bbq: q,
            stack,
            rx,
            net_id: None,
        }
    }

    /// Runs forever, processing incoming frames.
    ///
    /// The provided slice is used for receiving a frame via USB. It is used as the MTU
    /// for the entire connections.
    ///
    /// `max_usb_frame_size` is the largest size of USB frame we can receive. For example,
    /// it would be 64. This is NOT the largest message we can receive. It MUST be a power
    /// of two.
    pub async fn run(mut self, frame: &mut [u8], max_usb_frame_size: usize) -> ! {
        assert!(max_usb_frame_size.is_power_of_two());
        loop {
            self.rx.wait_enabled().await;
            info!("Connection established");

            // Mark the interface as established
            self.stack.with_interface_manager(|im| {
                im.inner.replace(EmbassyUsbManagerInner {
                    interface: ProducerHandle {
                        skt_tx: Interface {
                            prod: self.bbq.framed_producer(),
                            mtu: frame.len() as u16,
                        },
                    },
                    net_id: 0,
                })
            });

            // Handle all frames for the connection
            self.one_conn(frame, max_usb_frame_size).await;

            // Mark the connection as lost
            info!("Connection lost");
            self.stack.with_interface_manager(|im| {
                im.inner.take();
            });
        }
    }

    /// Handle all frames, returning when a connection error occurs
    async fn one_conn(&mut self, frame: &mut [u8], max_usb_frame_size: usize) {
        loop {
            match self.one_frame(frame, max_usb_frame_size).await {
                Ok(f) => {
                    // NOTE: this is BLOCKING, but does NOT wait for the request to
                    // be processed, we just copy the frame into its destination
                    // buffers.
                    //
                    // We COULD potentially gain some throughput by having another
                    // buffer here, so we can immediately begin receiving the next
                    // frame, at the cost of extra buffer space and copies.
                    self.process_frame(f);
                }
                Err(ReceiverError::ConnectionClosed) => break,
                Err(_e) => {
                    continue;
                }
            }
        }
    }

    /// Receive a single ergot frame, which might be across multiple reads of the endpoint
    ///
    /// No checking of the frame is done, only that the bulk endpoint gave us a frame.
    async fn one_frame<'a>(
        &mut self,
        frame: &'a mut [u8],
        max_frame_len: usize,
    ) -> Result<&'a mut [u8], ReceiverError> {
        let buflen = frame.len();
        let mut window = &mut frame[..];

        while !window.is_empty() {
            let n = match self.rx.read(window).await {
                Ok(n) => n,
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };

            let (_now, later) = window.split_at_mut(n);
            window = later;
            if n != max_frame_len {
                // We now have a full frame! Great!
                let wlen = window.len();
                let len = buflen - wlen;
                let frame = &mut frame[..len];

                return Ok(frame);
            }
        }

        // If we got here, we've run out of space. That's disappointing. Accumulate to the
        // end of this packet
        loop {
            match self.rx.read(frame).await {
                Ok(n) if n == max_frame_len => {}
                Ok(_) => return Err(ReceiverError::ReceivedMessageTooLarge),
                Err(EndpointError::BufferOverflow) => {
                    return Err(ReceiverError::ReceivedMessageTooLarge);
                }
                Err(EndpointError::Disabled) => return Err(ReceiverError::ConnectionClosed),
            };
        }
    }

    /// Perform the (immediate) validation and dispatching of a frame
    pub fn process_frame(&mut self, data: &mut [u8]) {
        let Some(mut frame) = de_frame(data) else {
            warn!(
                "Decode error! Ignoring frame on net_id {}",
                self.net_id.unwrap_or(0)
            );
            return;
        };

        debug!("Got Frame!");

        let take_net = self.net_id.is_none()
            || self
                .net_id
                .is_some_and(|n| frame.hdr.dst.network_id != 0 && n != frame.hdr.dst.network_id);

        if take_net {
            self.stack.with_interface_manager(|im| {
                if let Some(i) = im.inner.as_mut() {
                    // i am, whoever you say i am
                    warn!("Taking net {=u16}", frame.hdr.dst.network_id);
                    i.net_id = frame.hdr.dst.network_id;
                } else {
                    warn!("Whatttt");
                }
                // else: uhhhhhh
            });
            self.net_id = Some(frame.hdr.dst.network_id);
        }

        // If the message comes in and has a src net_id of zero,
        // we should rewrite it so it isn't later understood as a
        // local packet.
        //
        // TODO: accept any packet if we don't have a net_id yet?
        if let Some(net) = self.net_id.as_ref() {
            if frame.hdr.src.network_id == 0 {
                assert_ne!(frame.hdr.src.node_id, 0, "we got a local packet remotely?");
                assert_ne!(frame.hdr.src.node_id, 2, "someone is pretending to be us?");

                frame.hdr.src.network_id = *net;
            }
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
        use crate::NetStackSendError;
        match res {
            Ok(()) => {}
            Err(e) => {
                // TODO: match on error, potentially try to send NAK?
                match e {
                    NetStackSendError::SocketSend(_) => {
                        warn!("SocketSend(SocketSendError");
                    }
                    NetStackSendError::InterfaceSend(_) => {
                        warn!("InterfaceSend(InterfaceSendError");
                    }
                    NetStackSendError::NoRoute => {
                        warn!("NoRoute");
                    }
                    NetStackSendError::AnyPortMissingKey => {
                        warn!("AnyPortMissingKey");
                    }
                    NetStackSendError::WrongPortKind => {
                        warn!("WrongPortKind");
                    }
                    NetStackSendError::AnyPortNotUnique => {
                        warn!("AnyPortNotUnique");
                    }
                    NetStackSendError::AllPortMissingKey => {
                        warn!("AllPortMissingKey");
                    }
                }
            }
        }
    }
}

impl<R, D, const N: usize, C> Drop for Receiver<R, D, N, C>
where
    R: ScopedRawMutex + 'static,
    D: Driver<'static>,
    C: Coord,
{
    fn drop(&mut self) {
        // No receiver? Drop the interface.
        self.stack.with_interface_manager(|im| {
            im.inner.take();
        })
    }
}

/// Transmitter worker task
///
/// Takes a bbqueue from the NetStack of packets to send. While sending,
/// we will timeout if
pub async fn tx_worker<'a, D: Driver<'a>, const N: usize, C: Coord>(
    ep_in: &mut D::EndpointIn,
    rx: FramedConsumer<&'static BBQueue<Inline<N>, C, MaiNotSpsc>>,
    timeout_ms_per_frame: usize,
    max_usb_frame_size: usize,
) {
    assert!(max_usb_frame_size.is_power_of_two());
    info!("Started tx_worker");
    let mut pending = false;
    loop {
        // Wait for the endpoint to be connected...
        ep_in.wait_enabled().await;

        'connection: loop {
            // Wait for an outgoing frame
            let frame = rx.wait_read().await;

            // Attempt to send it
            let res = send_all::<D>(
                ep_in,
                &frame,
                &mut pending,
                timeout_ms_per_frame,
                max_usb_frame_size,
            )
            .await;

            // Done with this frame, free the buffer space
            frame.release();

            match res {
                Ok(()) => {}
                Err(TransmitError::Timeout) => {
                    // TODO: treat enough timeouts like a connection closed?
                    warn!("Transmit timeout!");
                }
                Err(TransmitError::ConnectionClosed) => break 'connection,
            }
        }

        // Drain any pending frames - the connection was lost.
        while let Ok(frame) = rx.read() {
            frame.release();
        }
    }
}

#[inline]
async fn send_all<'a, D>(
    ep_in: &mut D::EndpointIn,
    out: &[u8],
    pending_frame: &mut bool,
    timeout_ms_per_frame: usize,
    max_usb_frame_size: usize,
) -> Result<(), TransmitError>
where
    D: Driver<'a>,
{
    if out.is_empty() {
        return Ok(());
    }

    // Calculate an estimated timeout based on the number of frames we need to send
    // For now, we use 2ms/frame by default, rounded UP
    let frames = out.len().div_ceil(max_usb_frame_size);
    let timeout_ms = frames * timeout_ms_per_frame;

    let send_fut = async {
        // If we left off a pending frame, send one now so we don't leave an unterminated
        // message
        if *pending_frame && ep_in.write(&[]).await.is_err() {
            return Err(TransmitError::ConnectionClosed);
        }
        *pending_frame = true;

        // write in segments of max_usb_frame_size. The last chunk may
        // be 0 < len <= max_usb_frame_size.
        for ch in out.chunks(max_usb_frame_size) {
            if ep_in.write(ch).await.is_err() {
                return Err(TransmitError::ConnectionClosed);
            }
        }
        // If the total we sent was a multiple of max_usb_frame_size, send an
        // empty message to "flush" the transaction. We already checked
        // above that the len != 0.
        if (out.len() & (max_usb_frame_size - 1)) == 0 && ep_in.write(&[]).await.is_err() {
            return Err(TransmitError::ConnectionClosed);
        }

        *pending_frame = false;
        Ok(())
    };

    match select(send_fut, Timer::after_millis(timeout_ms as u64)).await {
        Either::First(res) => res,
        Either::Second(()) => Err(TransmitError::Timeout),
    }
}

// Helper bits

// ---- Constants ----

pub const DEVICE_INTERFACE_GUIDS: &[&str] = &["{AFB9A6FB-30BA-44BC-9232-806CFC875321}"];
/// Default time in milliseconds to wait for the completion of sending
pub const DEFAULT_TIMEOUT_MS_PER_FRAME: usize = 2;
/// Default max packet size for USB Full Speed
pub const USB_FS_MAX_PACKET_SIZE: usize = 64;

// ---- Statics ----

/// Statically store our packet buffers
static STINDX: AtomicU8 = AtomicU8::new(0xFF);
static HDLR: ConstStaticCell<ErgotHandler> = ConstStaticCell::new(ErgotHandler {});

// ---- Types ----

pub struct UsbDeviceBuffers<
    const CONFIG: usize = 256,
    const BOS: usize = 256,
    const CONTROL: usize = 64,
    const MSOS: usize = 256,
> {
    /// Config descriptor storage
    pub config_descriptor: [u8; CONFIG],
    /// BOS descriptor storage
    pub bos_descriptor: [u8; BOS],
    /// CONTROL endpoint buffer storage
    pub control_buf: [u8; CONTROL],
    /// MSOS descriptor buffer storage
    pub msos_descriptor: [u8; MSOS],
}

/// A helper type for `static` storage of buffers and driver components
pub struct WireStorage<
    const CONFIG: usize = 256,
    const BOS: usize = 256,
    const CONTROL: usize = 64,
    const MSOS: usize = 256,
> {
    /// Usb buffer storage
    pub bufs_usb: ConstStaticCell<UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>>,
    // /// WireTx/Sender static storage
    // pub cell: StaticCell<Mutex<M, EUsbWireTxInner<D>>>,
}

struct ErgotHandler {}

// ---- impls ----

// impl UsbDeviceBuffers

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize>
    UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>
{
    /// Create a new, empty set of buffers
    pub const fn new() -> Self {
        Self {
            config_descriptor: [0u8; CONFIG],
            bos_descriptor: [0u8; BOS],
            msos_descriptor: [0u8; MSOS],
            control_buf: [0u8; CONTROL],
        }
    }
}

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize> Default
    for UsbDeviceBuffers<CONFIG, BOS, CONTROL, MSOS>
{
    fn default() -> Self {
        Self::new()
    }
}

// impl WireStorage

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize>
    WireStorage<CONFIG, BOS, CONTROL, MSOS>
{
    /// Create a new, uninitialized static set of buffers
    pub const fn new() -> Self {
        Self {
            bufs_usb: ConstStaticCell::new(UsbDeviceBuffers::new()),
            // cell: StaticCell::new(),
        }
    }

    /// Initialize the static storage, reporting as ergot compatible
    ///
    /// This must only be called once.
    pub fn init_ergot<D: Driver<'static> + 'static>(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
    ) -> (UsbDevice<'static, D>, D::EndpointIn, D::EndpointOut) {
        let bufs = self.bufs_usb.take();

        let mut builder = Builder::new(
            driver,
            config,
            &mut bufs.config_descriptor,
            &mut bufs.bos_descriptor,
            &mut bufs.msos_descriptor,
            &mut bufs.control_buf,
        );

        // Register a ergot-compatible string handler
        let hdlr = HDLR.take();
        builder.handler(hdlr);

        // Add the Microsoft OS Descriptor (MSOS/MOD) descriptor.
        // We tell Windows that this entire device is compatible with the "WINUSB" feature,
        // which causes it to use the built-in WinUSB driver automatically, which in turn
        // can be used by libusb/rusb software without needing a custom driver or INF file.
        // In principle you might want to call msos_feature() just on a specific function,
        // if your device also has other functions that still use standard class drivers.
        builder.msos_descriptor(windows_version::WIN8_1, 0);
        builder.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
        builder.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
            "DeviceInterfaceGUIDs",
            msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
        ));

        // Add a vendor-specific function (class 0xFF), and corresponding interface,
        // that uses our custom handler.
        let mut function = builder.function(0xFF, 0, 0);
        let mut interface = function.interface();
        let stindx = interface.string();
        STINDX.store(stindx.0, Ordering::Relaxed);
        let mut alt = interface.alt_setting(0xFF, 0xCA, 0x7D, Some(stindx));
        let ep_out = alt.endpoint_bulk_out(64);
        let ep_in = alt.endpoint_bulk_in(64);
        drop(function);

        // Build the builder.
        let usb = builder.build();

        (usb, ep_in, ep_out)
    }

    /// Initialize the static storage.
    ///
    /// This must only be called once.
    pub fn init<D: Driver<'static> + 'static>(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
    ) -> (UsbDevice<'static, D>, D::EndpointIn, D::EndpointOut) {
        let (builder, wtx, wrx) = self.init_without_build(driver, config);
        let usb = builder.build();
        (usb, wtx, wrx)
    }
    /// Initialize the static storage, without building `Builder`
    ///
    /// This must only be called once.
    pub fn init_without_build<D: Driver<'static> + 'static>(
        &'static self,
        driver: D,
        config: embassy_usb::Config<'static>,
    ) -> (Builder<'static, D>, D::EndpointIn, D::EndpointOut) {
        let bufs = self.bufs_usb.take();

        let mut builder = Builder::new(
            driver,
            config,
            &mut bufs.config_descriptor,
            &mut bufs.bos_descriptor,
            &mut bufs.msos_descriptor,
            &mut bufs.control_buf,
        );

        // Add the Microsoft OS Descriptor (MSOS/MOD) descriptor.
        // We tell Windows that this entire device is compatible with the "WINUSB" feature,
        // which causes it to use the built-in WinUSB driver automatically, which in turn
        // can be used by libusb/rusb software without needing a custom driver or INF file.
        // In principle you might want to call msos_feature() just on a specific function,
        // if your device also has other functions that still use standard class drivers.
        builder.msos_descriptor(windows_version::WIN8_1, 0);
        builder.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
        builder.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
            "DeviceInterfaceGUIDs",
            msos::PropertyData::RegMultiSz(DEVICE_INTERFACE_GUIDS),
        ));

        // Add a vendor-specific function (class 0xFF), and corresponding interface,
        // that uses our custom handler.
        let mut function = builder.function(0xFF, 0, 0);
        let mut interface = function.interface();
        let mut alt = interface.alt_setting(0xFF, 0, 0, None);
        let ep_out = alt.endpoint_bulk_out(64);
        let ep_in = alt.endpoint_bulk_in(64);
        drop(function);

        (builder, ep_in, ep_out)
    }
}

impl<const CONFIG: usize, const BOS: usize, const CONTROL: usize, const MSOS: usize> Default
    for WireStorage<CONFIG, BOS, CONTROL, MSOS>
{
    fn default() -> Self {
        Self::new()
    }
}

// impl EUsbWireTx
// ...

// impl EUsbWireRx
// ...

// impl ErgotHandler

impl embassy_usb::Handler for ErgotHandler {
    fn get_string(&mut self, index: embassy_usb::types::StringIndex, lang_id: u16) -> Option<&str> {
        use embassy_usb::descriptor::lang_id;

        let stindx = STINDX.load(Ordering::Relaxed);
        if stindx == 0xFF {
            return None;
        }
        if lang_id == lang_id::ENGLISH_US && index.0 == stindx {
            Some("ergot")
        } else {
            None
        }
    }
}
