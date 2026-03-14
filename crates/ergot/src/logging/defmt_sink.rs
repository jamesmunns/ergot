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
//! Customize at build time:
//!
//! ```bash
//! DEFMT_SINK_BUFFER_SIZE=2048 cargo build
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
//! use ergot::{rtt_target, logging::defmt_sink};
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
//! use ergot::{rtt_target, logging::defmt_sink};
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

// ============================================================================
// bbqueue Network Support
// ============================================================================

#[cfg(feature = "defmt-sink-network")]
mod bbq {
    use bbqueue::{
        BBQueue,
        prod_cons::framed::{FramedConsumer, FramedGrantR, FramedGrantW},
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
    /// This is the max we'll try to acquire from the buffer for one log message.
    /// Frames larger than this will be dropped.
    const MAX_FRAME_SIZE: usize = if DEFMT_SINK_BUF_SIZE > 16 {
        // Leave some headroom for the framing header (2 bytes for u16)
        // and to allow multiple small frames in the buffer
        DEFMT_SINK_BUF_SIZE / 2
    } else {
        DEFMT_SINK_BUF_SIZE
    };

    /// BBQueue type for convenience
    type DefmtQueue = BBQueue<Inline<DEFMT_SINK_BUF_SIZE>, AtomicCoord, MaiNotSpsc>;

    /// Static BBQueue for defmt frames (framed mode)
    static BBQ: DefmtQueue = BBQueue::new();

    /// Frame accumulator for building complete defmt frames before committing.
    ///
    /// This solves the problem where defmt's encoder calls the write callback
    /// multiple times per log message. We accumulate all writes into a single
    /// bbqueue grant, then commit once at the end to create one complete frame.
    pub(super) struct FrameAccumulator {
        grant: FramedGrantW<&'static DefmtQueue, u16>,
        pos: usize,
        capacity: usize,
    }

    impl FrameAccumulator {
        /// Try to acquire a new frame accumulator.
        ///
        /// Returns `None` if the buffer is full or doesn't have enough space.
        pub fn try_new() -> Option<Self> {
            // Use u16 header to match the consumer
            let prod = <&DefmtQueue as BbqHandle>::framed_producer::<u16>(&&BBQ);
            // MAX_FRAME_SIZE is guaranteed to fit in u16 since it's at most DEFMT_SINK_BUF_SIZE/2
            let grant = prod.grant(MAX_FRAME_SIZE as u16).ok()?;
            let capacity = grant.len();
            Some(Self {
                grant,
                pos: 0,
                capacity,
            })
        }

        /// Append data to the frame being accumulated.
        ///
        /// If the frame would overflow, the extra data is silently dropped.
        /// This is better than panicking in an ISR context.
        pub fn write(&mut self, data: &[u8]) {
            let remaining = self.capacity - self.pos;
            let to_write = data.len().min(remaining);
            if to_write > 0 {
                self.grant[self.pos..self.pos + to_write].copy_from_slice(&data[..to_write]);
                self.pos += to_write;
            }
            // If data is truncated, we silently drop the overflow
        }

        /// Commit the accumulated frame and make it available to the consumer.
        ///
        /// This consumes the accumulator.
        pub fn commit(self) {
            // pos is guaranteed to fit in u16 since capacity <= MAX_FRAME_SIZE < u16::MAX
            self.grant.commit(self.pos as u16);
        }

        // Note: If the accumulator is dropped without calling commit(),
        // the underlying bbqueue grant's Drop impl will abort the frame automatically.
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

    use rtt_target::UpChannel;

    struct RttChannel {
        channel: UnsafeCell<Option<&'static mut UpChannel>>,
    }

    // SAFETY: Access is guarded by the defmt logger's critical section.
    unsafe impl Sync for RttChannel {}

    static RTT_CHANNEL: RttChannel = RttChannel {
        channel: UnsafeCell::new(None),
    };

    /// Set the RTT channel for defmt output.
    ///
    /// # Safety
    /// Must be called before any defmt logging and only once.
    pub(super) unsafe fn set_channel(channel: &'static mut UpChannel) {
        // SAFETY: Called once during init, before any logging occurs.
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
                #[cfg(feature = "defmt-sink-network")]
                {
                    *STATE.frame_accumulator.get() = super::bbq::FrameAccumulator::try_new();
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
pub fn init_with_options(opts: InitOptions) -> Option<DefmtConsumer> {
    #[cfg(feature = "defmt-sink-rtt")]
    if let Some(ch) = opts.rtt_channel {
        unsafe { rtt::set_channel(ch) };
    }

    #[cfg(feature = "defmt-sink-network")]
    {
        if opts.enable_network {
            return Some(bbq::init());
        }
    }

    None
}

/// Initialize the defmt sink with flexible outputs (RTT-only version).
///
/// This version is used when only RTT output is available (no network support).
/// Always returns `None` since there's no consumer for RTT-only operation.
#[cfg(all(feature = "defmt-sink-rtt", not(feature = "defmt-sink-network")))]
pub fn init_with_options(opts: InitOptions) -> Option<()> {
    if let Some(ch) = opts.rtt_channel {
        unsafe { rtt::set_channel(ch) };
    }
    None
}

/// Initialize network-only defmt sink (returns consumer for forwarding).
#[cfg(feature = "defmt-sink-network")]
pub fn init_network() -> DefmtConsumer {
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

/// Initialize RTT-only defmt sink (no network).
#[cfg(all(feature = "defmt-sink-rtt", not(feature = "defmt-sink-network")))]
pub fn init_rtt(rtt_channel: &'static mut rtt_target::UpChannel) {
    let _ = init_with_options(InitOptions {
        enable_network: false,
        rtt_channel: Some(rtt_channel),
    });
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
