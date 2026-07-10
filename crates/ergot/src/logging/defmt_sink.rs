//! defmt global_logger implementation for ergot
//!
//! This module provides a defmt Logger that sends raw defmt frames over multiple
//! outputs: ergot network (via bbqueue) and/or RTT (for debug probes).
//!
//! ## Feature Flags
//!
//! This module requires at least one of these features:
//!
//! - **`defmt-sink-network`**: Enables bbqueue buffering and network output.
//!   Provides async `DefmtConsumer` for forwarding frames to ergot network.
//!
//! - **`defmt-sink-rtt`**: Enables RTT channel output for direct probe debugging.
//!   Writes frames directly to RTT up channel.
//!
//! You can enable both features to get hybrid logging (local debug + remote network).
//!
//! ## Use Cases
//!
//! **Scenario 1: Network logging only**
//! ```toml
//! ergot = { features = ["defmt-sink-network"] }
//! ```
//!
//! **Scenario 2: RTT logging only**
//! ```toml
//! ergot = { features = ["defmt-sink-rtt"] }
//! ```
//! Result: defmt logs go only to RTT (no network)
//!
//! **Scenario 3: Hybrid (RTT + Network)**
//! ```toml
//! ergot = { features = ["defmt-sink-network", "defmt-sink-rtt"] }
//! ```
//!
//! **Scenario 4: Use your own defmt logger**
//! ```toml
//! ergot = { features = ["defmt-v1"] }
//! defmt-rtt = "0.4"
//! ```
//!
//! ## Buffer Size Configuration
//!
//! When using `defmt-sink-network`, the bbqueue buffer size defaults to 256 bytes.
//! The maximum size of a single defmt frame is half the buffer size (default: 128 bytes).
//! Frames that exceed this limit are silently dropped to avoid sending corrupted data.
//!
//! Typical defmt messages are very compact (5-20 bytes), so the defaults are fine
//! for most use cases. Increase the buffer if you log large byte arrays or structs.
//!
//! Set via environment variable:
//! ```bash
//! DEFMT_SINK_BUFFER_SIZE=1024 cargo build  # 1024 byte buffer, 512 byte max frame
//! ```
//!
//! Or in `.cargo/config.toml` (recommended for projects):
//! ```toml
//! [env]
//! DEFMT_SINK_BUFFER_SIZE = "1024"
//! ```
//!
//! ## Usage Examples
//!
//! ### Network Only
//!
//! ```ignore
//! use ergot::logging::defmt_sink;
//!
//! #[embassy_executor::main]
//! async fn main(spawner: Spawner) {
//!     // Initialize without RTT
//!     let consumer = defmt_sink::init_network();
//!
//!     // Spawn task to forward frames to network
//!     spawner.spawn(forward_defmt(consumer, &STACK));
//!
//!     defmt::info!("System started");
//! }
//!
//! async fn forward_defmt(consumer: DefmtConsumer, stack: &NetStack) {
//!     loop {
//!         let frame = consumer.wait_read().await;
//!         _ = stack.topics().broadcast_borrowed::<ErgotDefmtTxTopic>(
//!             &ErgotDefmtTx { frame: &frame },
//!             None,
//!         );
//!         frame.release();
//!     }
//! }
//! ```
//!
//! ### RTT + Network (Hybrid)
//!
//! ```ignore
//! use ergot::logging::defmt_sink;
//!
//! #[embassy_executor::main]
//! async fn main(spawner: Spawner) {
//!     // Set up RTT
//!     let channels = rtt_target::rtt_init! {
//!         up: { 0: { size: 4096, name: "defmt" } }
//!     };
//!
//!     // Initialize with RTT channel
//!     let consumer = defmt_sink::init_network_and_rtt(channels.up.0);
//!
//!     // Forward to network (RTT happens automatically)
//!     spawner.spawn(forward_defmt(consumer, &STACK));
//!
//!     defmt::info!("Logging to both RTT and network!");
//! }
//! ```
//!
//! ### RTT Only
//!
//! ```ignore
//! use ergot::logging::defmt_sink;
//!
//! fn main() {
//!     let channels = rtt_target::rtt_init! {
//!         up: { 0: { size: 1024, name: "defmt" } }
//!     };
//!
//!     defmt_sink::init_rtt(channels.up.0);
//!
//!     defmt::info!("Logging to RTT only");
//! }
//! ```

use core::sync::atomic::{AtomicBool, Ordering};

/// Runtime flag: whether network output (bbqueue) is active.
/// Only set to `true` when `init_network()` or `init_network_and_rtt()` is called.
/// When `false`, the logger skips bbqueue allocation entirely — no overhead.
#[cfg(feature = "defmt-sink-network")]
static NETWORK_ENABLED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// bbqueue Network Support
// ============================================================================

#[cfg(feature = "defmt-sink-network")]
mod bbq {
    use bbqueue::{
        BBQueue,
        prod_cons::framed::{FramedConsumer, FramedGrantR},
        traits::{
            bbqhdl::BbqHandle, coordination::cas::AtomicCoord, notifier::maitake::MaiNotSpsc,
            storage::Inline,
        },
    };

    // Buffer size configuration (from build.rs)
    mod consts {
        include!(concat!(env!("OUT_DIR"), "/defmt_sink_consts.rs"));
    }
    pub use consts::DEFMT_SINK_BUF_SIZE;

    /// Maximum size for a single defmt frame.
    ///
    /// Frames that exceed this are silently dropped (truncated frames would
    /// corrupt the host-side stream decoder).
    ///
    /// Strictly BELOW half the ring, not exactly half: a framed grant of
    /// `n` payload bytes needs `n + 2` (u16 header) CONTIGUOUS bytes, and
    /// on an empty ring parked at write offset `w` that fits either in the
    /// tail (`w <= Q - n - 2`) or via wrap (`n + 2 <= w`). The two windows
    /// cover every `w` iff `n + 2 <= Q/2` — so capping at `Q/2 - 4` makes
    /// every accepted frame placeable at ANY pointer position, and the
    /// only remaining drop cause is a genuinely full queue (consumer
    /// behind). At exactly `Q/2` there is a ~4-byte dead band of pointer
    /// positions where an empty ring cannot take the frame.
    pub(super) const MAX_FRAME_SIZE: usize = if DEFMT_SINK_BUF_SIZE > 16 {
        DEFMT_SINK_BUF_SIZE / 2 - 4
    } else {
        DEFMT_SINK_BUF_SIZE
    };

    /// BBQueue type for convenience
    type DefmtQueue = BBQueue<Inline<DEFMT_SINK_BUF_SIZE>, AtomicCoord, MaiNotSpsc>;

    /// Static BBQueue for defmt frames (framed mode)
    static BBQ: DefmtQueue = BBQueue::new();

    /// Scratch buffer for accumulating one defmt frame before the bbqueue
    /// grant. Accessed ONLY between `Logger::acquire()` and
    /// `Logger::release()`, which hold a critical section and assert
    /// non-reentrancy — so this is effectively a single-owner buffer.
    static mut SCRATCH: [u8; MAX_FRAME_SIZE] = [0; MAX_FRAME_SIZE];

    /// Frame accumulator for building complete defmt frames before committing.
    ///
    /// This solves the problem where defmt's encoder calls the write callback
    /// multiple times per log message: writes accumulate into a scratch
    /// buffer, and `commit()` takes a bbqueue grant of EXACTLY the frame
    /// size.
    ///
    /// Why not grant `MAX_FRAME_SIZE` up front and commit less (the original
    /// design): a framed grant needs CONTIGUOUS ring space, and a
    /// half-buffer grant has a geometric dead zone — for certain write/read
    /// pointer positions neither the tail nor the wrapped head can fit
    /// buf/2 contiguous bytes even though the ring is EMPTY. Since only
    /// successful grants advance the pointer, parking in the dead zone was
    /// PERMANENT: every subsequent frame failed the grant and the network
    /// sink went silent forever (observed on STM32F405 + USB transport,
    /// 2026-07-07: grant_fail counting up at the exact log rate, zero
    /// commits, from boot until reset). Exact-size grants keep the pointer
    /// moving and have no practical dead zone at defmt frame sizes.
    pub(super) struct FrameAccumulator {
        pos: usize,
        truncated: bool,
    }

    impl FrameAccumulator {
        /// Start accumulating a new frame.
        ///
        /// Must only be called from `Logger::acquire()` (critical section
        /// held, non-reentrant) — see `SCRATCH`.
        pub fn try_new() -> Option<Self> {
            Some(Self {
                pos: 0,
                truncated: false,
            })
        }

        /// Append data to the frame being accumulated.
        ///
        /// If the frame would overflow, the frame is marked truncated and
        /// dropped at commit (a corrupted defmt frame is worse than a lost
        /// one).
        pub fn write(&mut self, data: &[u8]) {
            let remaining = MAX_FRAME_SIZE - self.pos;
            if data.len() > remaining {
                self.truncated = true;
            }
            let to_write = data.len().min(remaining);
            if to_write > 0 {
                // SAFETY: see `SCRATCH` — exclusive access is guaranteed by
                // the defmt logger's critical section + reentrancy check.
                unsafe {
                    let scratch = &mut *core::ptr::addr_of_mut!(SCRATCH);
                    scratch[self.pos..self.pos + to_write].copy_from_slice(&data[..to_write]);
                }
                self.pos += to_write;
            }
        }

        /// Grant exactly `pos` bytes, copy the frame in, and commit.
        pub fn commit(self) {
            if self.truncated || self.pos == 0 {
                return;
            }
            let prod = <&DefmtQueue as BbqHandle>::framed_producer::<u16>(&&BBQ);
            // pos <= MAX_FRAME_SIZE <= u16::MAX by construction.
            let Ok(mut grant) = prod.grant(self.pos as u16) else {
                // Queue genuinely full (consumer behind) — drop this frame.
                return;
            };
            // SAFETY: see `SCRATCH`.
            unsafe {
                let scratch = &*core::ptr::addr_of!(SCRATCH);
                grant[..self.pos].copy_from_slice(&scratch[..self.pos]);
            }
            grant.commit(self.pos as u16);
        }
    }

    /// Initialize bbqueue and return consumer
    pub(super) fn init() -> DefmtConsumer {
        DefmtConsumer {
            inner: <&DefmtQueue as BbqHandle>::framed_consumer(&&BBQ),
        }
    }

    /// Consumer for reading defmt frames asynchronously
    pub struct DefmtConsumer {
        inner: FramedConsumer<&'static DefmtQueue, u16>,
    }

    impl DefmtConsumer {
        /// Async wait for next frame
        ///
        /// Returns a grant. Call `.release()` when done to free the buffer space.
        #[must_use = "The grant must be released to free buffer space"]
        pub async fn wait_read(&self) -> FramedGrantR<&'static DefmtQueue, u16> {
            self.inner.wait_read().await
        }

        /// Try to read a frame without waiting
        ///
        /// Returns a grant. Call `.release()` when done to free the buffer space.
        #[must_use = "The grant must be released to free buffer space"]
        pub fn read(
            &self,
        ) -> Result<
            FramedGrantR<&'static DefmtQueue, u16>,
            bbqueue::traits::coordination::ReadGrantError,
        > {
            self.inner.read()
        }
    }
}

#[cfg(feature = "defmt-sink-network")]
pub use bbq::DefmtConsumer;

// ============================================================================
// RTT Support
// ============================================================================

#[cfg(feature = "defmt-sink-rtt")]
mod rtt {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    use rtt_target::UpChannel;

    struct RttChannel {
        channel: UnsafeCell<Option<&'static mut UpChannel>>,
    }

    // SAFETY: Access is guarded by the defmt logger's critical section.
    unsafe impl Sync for RttChannel {}

    static RTT_CHANNEL: RttChannel = RttChannel {
        channel: UnsafeCell::new(None),
    };

    static RTT_INIT: AtomicBool = AtomicBool::new(false);

    /// Set the RTT channel for defmt output.
    ///
    /// # Safety
    /// Must be called before any defmt logging and only once.
    ///
    /// # Panics
    /// Panics if called more than once (double-init would create aliasing `&'static mut`).
    pub(super) unsafe fn set_channel(channel: &'static mut UpChannel) {
        assert!(
            !RTT_INIT.swap(true, Ordering::SeqCst),
            "defmt RTT channel already initialized — set_channel() must only be called once"
        );
        // SAFETY: Called once during init, before any logging occurs.
        // The AtomicBool guard above prevents double-init.
        unsafe {
            *RTT_CHANNEL.channel.get() = Some(channel);
        }
    }

    /// Write data to the RTT channel.
    ///
    /// # Safety
    /// Must be called within the defmt logger's critical section.
    pub(super) unsafe fn write_data(data: &[u8]) {
        // SAFETY: Accessed only within the defmt logger's critical section.
        unsafe {
            if let Some(ref mut ch) = *RTT_CHANNEL.channel.get() {
                ch.write(data);
            }
        }
    }
}

// ============================================================================
// Global Logger Implementation
// ============================================================================

#[cfg(any(feature = "defmt-sink-network", feature = "defmt-sink-rtt"))]
mod logger {
    use core::cell::UnsafeCell;

    use super::*;

    #[defmt::global_logger]
    struct Logger;

    /// Logger state, wrapped in `UnsafeCell` to avoid `static mut`.
    ///
    /// All fields are only accessed within a critical section acquired in
    /// `Logger::acquire()` and released in `Logger::release()`, so concurrent
    /// access is impossible.
    struct LoggerState {
        taken: AtomicBool,
        cs_restore: UnsafeCell<critical_section::RestoreState>,
        encoder: UnsafeCell<defmt::Encoder>,
        #[cfg(feature = "defmt-sink-network")]
        frame_accumulator: UnsafeCell<Option<super::bbq::FrameAccumulator>>,
    }

    // SAFETY: All UnsafeCell fields are only accessed within the defmt logger's
    // critical section. The `taken` AtomicBool prevents reentrant access.
    unsafe impl Sync for LoggerState {}

    impl LoggerState {
        const fn new() -> Self {
            Self {
                taken: AtomicBool::new(false),
                cs_restore: UnsafeCell::new(critical_section::RestoreState::invalid()),
                encoder: UnsafeCell::new(defmt::Encoder::new()),
                #[cfg(feature = "defmt-sink-network")]
                frame_accumulator: UnsafeCell::new(None),
            }
        }
    }

    static STATE: LoggerState = LoggerState::new();

    /// Combined write function - writes to all enabled outputs.
    ///
    /// # Safety
    /// Must be called within the defmt logger's critical section.
    fn do_write(data: &[u8]) {
        // Write to RTT if enabled (immediate, no accumulation needed)
        #[cfg(feature = "defmt-sink-rtt")]
        // SAFETY: Called within the logger's critical section.
        unsafe {
            super::rtt::write_data(data);
        }

        // Write to network accumulator if enabled
        #[cfg(feature = "defmt-sink-network")]
        // SAFETY: Accessed within the logger's critical section.
        unsafe {
            if let Some(ref mut acc) = *STATE.frame_accumulator.get() {
                acc.write(data);
            }
            // If no accumulator (buffer was full), silently drop
        }
    }

    unsafe impl defmt::Logger for Logger {
        fn acquire() {
            // Acquire critical section
            let restore = unsafe { critical_section::acquire() };

            // Check for reentrancy
            if STATE.taken.load(Ordering::Relaxed) {
                panic!("defmt logger taken reentrantly")
            }

            STATE.taken.store(true, Ordering::Relaxed);

            // SAFETY: We have acquired a critical section and verified non-reentrancy.
            unsafe {
                *STATE.cs_restore.get() = restore;

                // Try to acquire a frame accumulator for network output
                // (only if network was initialized via init_network/init_network_and_rtt)
                #[cfg(feature = "defmt-sink-network")]
                {
                    *STATE.frame_accumulator.get() =
                        if super::NETWORK_ENABLED.load(Ordering::Relaxed) {
                            super::bbq::FrameAccumulator::try_new()
                        } else {
                            None
                        };
                }

                (*STATE.encoder.get()).start_frame(do_write);
            }
        }

        unsafe fn flush() {
            // RTT handles flushing internally, bbqueue doesn't need flushing
        }

        unsafe fn release() {
            // SAFETY: Called with the critical section held from acquire().
            unsafe {
                (*STATE.encoder.get()).end_frame(do_write);

                // Commit the accumulated network frame
                #[cfg(feature = "defmt-sink-network")]
                {
                    if let Some(acc) = (*STATE.frame_accumulator.get()).take() {
                        acc.commit();
                    }
                }

                STATE.taken.store(false, Ordering::Relaxed);

                let restore = *STATE.cs_restore.get();
                critical_section::release(restore);
            }
        }

        unsafe fn write(bytes: &[u8]) {
            // SAFETY: Called with the critical section held from acquire().
            unsafe {
                (*STATE.encoder.get()).write(bytes, do_write);
            }
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Options for initializing the defmt sink.
///
/// - `enable_network`: when `defmt-sink-network` is compiled in, forward frames
///   into the bbqueue queue for later network forwarding.
/// - `rtt_channel`: when `defmt-sink-rtt` is compiled in, also write frames to
///   the given RTT up channel (hybrid or RTT-only).
#[cfg(any(feature = "defmt-sink-network", feature = "defmt-sink-rtt"))]
#[derive(Default)]
pub struct InitOptions {
    /// Enable bbqueue queueing for network forwarding (if available).
    pub enable_network: bool,
    /// Optional RTT up channel for direct probe output (hybrid or RTT-only).
    #[cfg(feature = "defmt-sink-rtt")]
    pub rtt_channel: Option<&'static mut rtt_target::UpChannel>,
}

impl InitOptions {
    /// Convenience constructor with network forwarding enabled and no RTT output.
    pub const fn network_only() -> Self {
        Self {
            enable_network: true,
            #[cfg(feature = "defmt-sink-rtt")]
            rtt_channel: None,
        }
    }
}

/// Initialize the defmt sink with flexible outputs.
///
/// When `defmt-sink-network` is enabled:
/// - Returns `Some(DefmtConsumer)` when network forwarding is requested
/// - Returns `None` when network forwarding is disabled
///
/// When only `defmt-sink-rtt` is enabled:
/// - Always returns `None` (no consumer needed for RTT-only operation)
///
/// RTT output is set up when an RTT channel is provided and the `defmt-sink-rtt`
/// feature is active.
#[cfg(feature = "defmt-sink-network")]
pub(crate) fn init_with_options(opts: InitOptions) -> Option<DefmtConsumer> {
    #[cfg(feature = "defmt-sink-rtt")]
    if let Some(ch) = opts.rtt_channel {
        unsafe { rtt::set_channel(ch) };
    }

    if opts.enable_network {
        NETWORK_ENABLED.store(true, Ordering::Relaxed);
        return Some(bbq::init());
    }

    None
}

/// Initialize the defmt sink with flexible outputs (RTT-only version).
///
/// This version is used when only RTT output is available (no network support).
/// Always returns `None` since there's no consumer for RTT-only operation.
#[cfg(all(feature = "defmt-sink-rtt", not(feature = "defmt-sink-network")))]
pub(crate) fn init_with_options(opts: InitOptions) -> Option<()> {
    if let Some(ch) = opts.rtt_channel {
        unsafe { rtt::set_channel(ch) };
    }
    None
}

/// Initialize network-only defmt sink (returns consumer for forwarding).
#[cfg(feature = "defmt-sink-network")]
pub fn init_network() -> DefmtConsumer {
    NETWORK_ENABLED.store(true, Ordering::Relaxed);
    bbq::init()
}

/// Initialize hybrid network + RTT defmt sink.
#[cfg(all(feature = "defmt-sink-network", feature = "defmt-sink-rtt"))]
pub fn init_network_and_rtt(rtt_channel: &'static mut rtt_target::UpChannel) -> DefmtConsumer {
    init_with_options(InitOptions {
        enable_network: true,
        rtt_channel: Some(rtt_channel),
    })
    .expect("network sink not compiled in")
}

/// Initialize RTT-only defmt sink (no network output).
///
/// Works even when `defmt-sink-network` is compiled in — the bbqueue is
/// simply not activated, so there's zero overhead from the network path.
#[cfg(feature = "defmt-sink-rtt")]
pub fn init_rtt(rtt_channel: &'static mut rtt_target::UpChannel) {
    unsafe { rtt::set_channel(rtt_channel) };
}

// ============================================================================
// Forwarding helpers
// ============================================================================

/// Forward frames from the defmt sink to the standard defmt topic over ergot.
///
/// This is a convenience loop that waits for frames, broadcasts them as
/// [`ErgotDefmtTx`](crate::logging::defmtlog::ErgotDefmtTx) messages, and
/// releases the underlying buffer grants.
#[cfg(all(feature = "defmt-sink-network", feature = "defmtlog"))]
pub async fn forward_to_ergot_topic<NS>(consumer: &DefmtConsumer, stack: NS, name: Option<&str>)
where
    NS: crate::net_stack::NetStackHandle,
{
    loop {
        let frame = consumer.wait_read().await;
        let _ = stack
            .stack()
            .topics()
            .broadcast_borrowed::<crate::well_known::ErgotDefmtTxTopic>(
                &crate::logging::defmtlog::ErgotDefmtTx { frame: &frame },
                name,
            );
        frame.release();
    }
}

#[cfg(all(test, feature = "defmt-sink-network"))]
mod tests {
    use super::bbq;

    /// Push one frame through the accumulator; returns true if it landed in
    /// the queue (commit is infallible-looking, so observe via the consumer).
    ///
    /// NB: this drives `FrameAccumulator` (and thus `SCRATCH`) outside the
    /// defmt `Logger::acquire`/`release` critical section that guarantees
    /// exclusivity in production. That is sound here only because the test
    /// is single-threaded and no defmt logger is active — accumulators are
    /// created and committed strictly sequentially.
    fn push(consumer: &bbq::DefmtConsumer, payload: &[u8]) -> bool {
        let mut acc = bbq::FrameAccumulator::try_new().unwrap();
        acc.write(payload);
        acc.commit();
        match consumer.read() {
            Ok(grant) => {
                assert_eq!(&grant[..], payload, "frame must round-trip intact");
                grant.release();
                true
            }
            Err(_) => false,
        }
    }

    /// Regression for the half-ring dead zone (fixed 2026-07-08): the old
    /// design granted MAX_FRAME_SIZE (= ring/2) per frame, and an empty
    /// ring parked with its pointers at exactly ring/2 could satisfy that
    /// grant neither in the tail nor via wrap — every subsequent frame
    /// dropped, FOREVER. Exact-size grants (+ the MAX cap strictly below
    /// half the ring) must survive that park and every other position.
    ///
    /// One test fn on purpose: the queue is a global static, parallel test
    /// fns would interleave.
    #[test]
    fn frames_survive_any_ring_position() {
        let consumer = bbq::init();
        let q = bbq::DEFMT_SINK_BUF_SIZE;

        // Park the ring exactly at q/2: two fillers of q/4 each (payload
        // q/4 - 2 + u16 header), written and drained — ring is EMPTY with
        // both pointers at the historical dead spot.
        let filler = vec![0xA5u8; q / 4 - 2];
        assert!(push(&consumer, &filler));
        assert!(push(&consumer, &filler));

        // The killer case: a maximum-size frame at the q/2 park. Under the
        // old half-ring grant this failed forever.
        let max = vec![0x5Au8; bbq::MAX_FRAME_SIZE];
        assert!(
            push(&consumer, &max),
            "max-size frame must fit on an empty ring parked at q/2"
        );

        // Sweep: mixed sizes drive the pointers through wraps and odd
        // offsets; every frame must land (ring is drained each time, so
        // the only possible failure is grant geometry).
        for i in 0..200usize {
            let len = 1 + (i * 37) % bbq::MAX_FRAME_SIZE;
            let payload: Vec<u8> = (0..len).map(|b| (b ^ i) as u8).collect();
            assert!(
                push(&consumer, &payload),
                "frame {i} (len {len}) dropped on a drained ring"
            );
        }
    }
}
