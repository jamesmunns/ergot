//! defmt global_logger implementation for ergot
//!
//! This module provides a defmt Logger that sends raw defmt frames over the
//! ergot network. Similar to the `log_v0_4` module's approach, this prevents
//! infinite loops by using a GEID (Get Ergot Internal Defmt-logger) system.
//!
//! ## Feature Flags
//!
//! This module is only available with the **`defmt-sink`** feature flag.
//!
//! - **`defmt-v1`**: Enables defmt::Format on ergot types + defmt message types
//! - **`defmt-sink`**: Enables THIS module (DefmtSink implementation)
//!
//! ## Use Cases
//!
//! **Scenario 1: Use your own defmt logger (e.g., defmt-rtt)**
//! ```toml
//! ergot = { version = "0.12", features = ["defmt-v1"] }  # NOT defmt-sink
//! defmt-rtt = "0.4"
//! ```
//! Result: ergot's types have defmt::Format, ergot's internal logs go to defmt-rtt
//!
//! **Scenario 2: Use ergot as a network-based defmt logger**
//! ```toml
//! ergot = { version = "0.12", features = ["defmt-sink"] }
//! ```
//! Result: All defmt logs (including ergot's) are sent over the ergot network
//!
//! **Scenario 3: Receive defmt logs from network on host**
//! ```toml
//! ergot = { version = "0.12", features = ["defmt-v1", "tokio-std"] }
//! ```
//! Result: Can subscribe to ErgotDefmtRxTopic and decode incoming defmt frames
//!
//! ## Architecture Overview
//!
//! * Which defmt sink does ERGOT use for logging? Valid choices are:
//!   * A user provided logger (e.g., defmt-rtt, defmt-itm)
//!   * A null logger (compiler removes defmt calls)
//! * Should ergot serve as the global defmt logger?
//!
//! For Ergot's defmt logger:
//!
//! * If !ERGOT_GLOBAL_SET && !ERGOT_SPECIFIC_SET: defmt is not used internally
//! * If ERGOT_GLOBAL_SET && !ERGOT_SPECIFIC_SET: defmt calls are compiled out
//! * If ERGOT_SPECIFIC_SET: use user's defmt logger, hope it isn't ergot
//!
//! ## Complete Usage Example
//!
//! ### On the Embedded Device (Sender)
//!
//! ```ignore
//! use ergot::{
//!     logging::defmt_v1::DefmtSink,
//!     logging::defmtlog::ErgotDefmtTx,
//!     well_known::ErgotDefmtTxTopic,
//!     NetStack,
//! };
//!
//! // Your ergot network stack
//! static STACK: NetStack<...> = ...;
//!
//! // Define the global logger
//! #[defmt::global_logger]
//! struct GlobalLogger;
//!
//! unsafe impl defmt::Logger for GlobalLogger {
//!     fn acquire() {
//!         DefmtSink::acquire()
//!     }
//!     unsafe fn flush() {
//!         DefmtSink::flush()
//!     }
//!     unsafe fn release() {
//!         DefmtSink::release()
//!     }
//!     unsafe fn write(bytes: &[u8]) {
//!         DefmtSink::write(bytes)
//!     }
//! }
//!
//! // In your main function, initialize the sink with a send function
//! #[embassy_executor::main]
//! async fn main() {
//!     // Initialize the defmt sink
//!     DefmtSink::init_with_sender(|frame| {
//!         _ = STACK.topics().broadcast_borrowed::<ErgotDefmtTxTopic>(
//!             &ErgotDefmtTx { frame },
//!             None,
//!         );
//!     });
//!
//!     // Now you can use defmt logging!
//!     defmt::info!("System initialized, temp={}", temperature);
//!     defmt::warn!("Low battery: {}%", battery_level);
//! }
//! ```
//!
//! ### On the Host/Controller (Receiver)
//!
//! ```ignore
//! use ergot::{
//!     logging::defmtlog::ErgotDefmtRx,
//!     well_known::ErgotDefmtRxTopic,
//! };
//!
//! // Subscribe to defmt frames
//! let mut rx = stack.subscribe::<ErgotDefmtRxTopic>(None)?;
//!
//! // Receive and decode frames
//! while let Ok(frame_msg) = rx.recv().await {
//!     let frame = frame_msg.frame;
//!
//!     // Decode using defmt-decoder + ELF file
//!     // (defmt-decoder crate provides the decoder implementation)
//!     match decoder.decode(frame) {
//!         Ok(decoded) => println!("{}", decoded),
//!         Err(e) => eprintln!("Failed to decode frame: {}", e),
//!     }
//! }
//! ```

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use critical_section::CriticalSection;

use crate::{
    logging::defmtlog::ErgotDefmtTx, net_stack::NetStackHandle, traits::Topic,
    well_known::ErgotDefmtTxTopic,
};

/// Maximum size of a single defmt frame
///
/// This should be large enough for most log messages. defmt frames are
/// typically quite small (10-50 bytes for simple logs, up to a few hundred
/// for complex ones). If a frame exceeds this size, it will be truncated.
const MAX_FRAME_SIZE: usize = 512;

/// Frame buffer storage
struct FrameBuffer {
    /// The buffer for the current frame being constructed
    buffer: UnsafeCell<[u8; MAX_FRAME_SIZE]>,
    /// Current position in the buffer
    pos: UnsafeCell<usize>,
    /// Whether the logger is currently acquired
    acquired: AtomicBool,
}

unsafe impl Sync for FrameBuffer {}

impl FrameBuffer {
    const fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([0u8; MAX_FRAME_SIZE]),
            pos: UnsafeCell::new(0),
            acquired: AtomicBool::new(false),
        }
    }

    /// Reset the buffer for a new frame
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired
    unsafe fn reset(&self) {
        unsafe {
            *self.pos.get() = 0;
        }
    }

    /// Write bytes to the buffer
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired
    unsafe fn write(&self, bytes: &[u8]) {
        unsafe {
            let pos = &mut *self.pos.get();
            let buffer = &mut *self.buffer.get();

            let remaining = MAX_FRAME_SIZE.saturating_sub(*pos);
            let to_copy = bytes.len().min(remaining);

            if to_copy > 0 {
                buffer[*pos..*pos + to_copy].copy_from_slice(&bytes[..to_copy]);
                *pos += to_copy;
            }

            // If we couldn't fit all bytes, silently truncate
            // (defmt's write() is not allowed to fail)
        }
    }

    /// Get the current frame as a slice
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired
    unsafe fn frame(&self) -> &[u8] {
        unsafe {
            let pos = *self.pos.get();
            let buffer = &*self.buffer.get();
            &buffer[..pos]
        }
    }
}

static FRAME_BUFFER: FrameBuffer = FrameBuffer::new();

/// Type-erased send function
///
/// This function pointer is set at initialization and is used to send
/// defmt frames over the ergot network without needing to know the
/// concrete NetStack type.
type SendFn = fn(&[u8]);

/// The send function that defmt will use for sending frames
///
/// This is set once at initialization time by calling `init()`
struct StaticSendFn {
    send_fn: UnsafeCell<Option<SendFn>>,
    initialized: AtomicBool,
}

unsafe impl Sync for StaticSendFn {}

impl StaticSendFn {
    const fn new() -> Self {
        Self {
            send_fn: UnsafeCell::new(None),
            initialized: AtomicBool::new(false),
        }
    }

    /// Initialize with a send function
    ///
    /// This should be called once, before any defmt logging occurs.
    /// Subsequent calls are ignored.
    fn init(&'static self, send_fn: SendFn) {
        critical_section::with(|_cs| {
            if !self.initialized.load(Ordering::Acquire) {
                unsafe {
                    *self.send_fn.get() = Some(send_fn);
                }
                self.initialized.store(true, Ordering::Release);
            }
        });
    }

    /// Call the send function, if initialized
    fn send(&self, frame: &[u8]) {
        if self.initialized.load(Ordering::Acquire) {
            unsafe {
                if let Some(send_fn) = *self.send_fn.get() {
                    send_fn(frame);
                }
            }
        }
    }
}

static DEFMT_SEND: StaticSendFn = StaticSendFn::new();

/// Internal defmt logger state management (GEID = Get Ergot Internal Defmt-logger)
pub(crate) mod internal {
    use super::*;

    pub(super) struct StaticDefmtState {
        state: AtomicU8,
    }

    impl StaticDefmtState {
        pub(super) const fn new() -> Self {
            Self {
                state: AtomicU8::new(GEID_STATE_ENABLED),
            }
        }

        /// Disable internal defmt logging (when ergot becomes the global logger)
        pub(super) fn set_disabled_with_cs(&'static self, _cs: CriticalSection<'_>) {
            // Only allow ENABLED -> DISABLED to prevent toggling
            if self.state.load(Ordering::Relaxed) != GEID_STATE_ENABLED {
                return;
            }
            self.state.store(GEID_STATE_DISABLED, Ordering::Relaxed);
        }

        #[cfg(target_has_atomic = "8")]
        pub(super) fn set_disabled_atomic(&'static self) {
            // Only allow ENABLED -> DISABLED
            _ = self.state.compare_exchange(
                GEID_STATE_ENABLED,
                GEID_STATE_DISABLED,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
        }

        /// Check if internal defmt logging is enabled
        #[inline]
        pub(crate) fn is_enabled(&self) -> bool {
            self.state.load(Ordering::Acquire) == GEID_STATE_ENABLED
        }
    }

    pub(super) static GEID_STORE_GLOBAL: StaticDefmtState = StaticDefmtState::new();
    const GEID_STATE_ENABLED: u8 = 0;
    const GEID_STATE_DISABLED: u8 = 1;

    /// Check if ergot's internal defmt logging should be active
    ///
    /// This should be used with conditional compilation in ergot's internal code:
    /// ```ignore
    /// #[cfg(feature = "defmt-v1")]
    /// if internal::geid_enabled() {
    ///     defmt::info!("ergot internal message");
    /// }
    /// ```
    #[inline]
    pub(crate) fn geid_enabled() -> bool {
        GEID_STORE_GLOBAL.is_enabled()
    }
}

/// DefmtSink provides static methods for implementing defmt::Logger
///
/// This struct doesn't implement the Logger trait directly because defmt requires
/// a unit struct marked with #[global_logger]. Instead, users create their own
/// global_logger that delegates to these static methods.
///
/// ## Example
///
/// ```ignore
/// #[defmt::global_logger]
/// struct GlobalLogger;
///
/// unsafe impl defmt::Logger for GlobalLogger {
///     fn acquire() {
///         DefmtSink::acquire(&STACK)
///     }
///     unsafe fn flush() {
///         DefmtSink::flush()
///     }
///     unsafe fn release() {
///         DefmtSink::release()
///     }
///     unsafe fn write(bytes: &[u8]) {
///         DefmtSink::write(bytes)
///     }
/// }
/// ```
pub struct DefmtSink;

impl DefmtSink {
    /// Initialize the defmt sink with a send function
    ///
    /// This must be called before any defmt logging occurs. It's safe to
    /// call multiple times; subsequent calls are ignored.
    ///
    /// The send function should broadcast the defmt frame over the ergot network.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// DefmtSink::init_with_sender(|frame| {
    ///     _ = STACK.topics().broadcast_borrowed::<ErgotDefmtTxTopic>(
    ///         &ErgotDefmtTx { frame },
    ///         None,
    ///     );
    /// });
    /// ```
    pub fn init_with_sender(send_fn: fn(&[u8])) {
        DEFMT_SEND.init(send_fn);
    }

    /// Acquire the logger (called by defmt before logging)
    ///
    /// This uses a critical section to ensure thread/interrupt safety.
    /// It will panic if the logger is already acquired (re-entrant logging).
    pub fn acquire() {
        critical_section::with(|_cs| {
            // Check if already acquired (would indicate re-entrant logging)
            if FRAME_BUFFER
                .acquired
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                panic!("defmt logger re-entrancy detected");
            }

            // Reset the buffer for a new frame
            unsafe {
                FRAME_BUFFER.reset();
            }

            // Initialize the defmt Encoder
            unsafe {
                defmt::export::acquire();
            }
        });
    }

    /// Write bytes to the frame buffer (called by defmt during logging)
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired.
    pub unsafe fn write(bytes: &[u8]) {
        // Early return if we're in a bad state (shouldn't happen)
        if !FRAME_BUFFER.acquired.load(Ordering::Acquire) {
            return;
        }

        // defmt v1.0 handles encoding internally, we just write the bytes
        FRAME_BUFFER.write(bytes);

        // Forward to defmt's internal write
        defmt::export::write(bytes);
    }

    /// Flush the logger (called by defmt during logging)
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired.
    ///
    /// Note: We don't actually flush anything here since the consumer
    /// is in userspace and there's no meaningful flush operation.
    pub unsafe fn flush() {
        // No-op: the frame will be sent in release()
    }

    /// Release the logger (called by defmt after logging)
    ///
    /// This finalizes the frame and sends it over the ergot network.
    ///
    /// # Safety
    ///
    /// Must only be called when the logger is acquired.
    pub unsafe fn release() {
        // Finalize defmt's encoder
        defmt::export::release();

        // Get the complete frame
        let frame = FRAME_BUFFER.frame();

        // Send it over ergot using the registered send function
        DEFMT_SEND.send(frame);

        // Release the lock
        FRAME_BUFFER.acquired.store(false, Ordering::Release);
    }

    /// Disable ergot's internal defmt logging
    ///
    /// This is automatically called when registering DefmtSink as the global
    /// logger to prevent infinite loops. You typically don't need to call this
    /// manually.
    #[cfg(not(feature = "std"))]
    pub fn disable_internal_with_cs(cs: CriticalSection<'_>) {
        internal::GEID_STORE_GLOBAL.set_disabled_with_cs(cs);
    }

    /// Disable ergot's internal defmt logging (atomic version)
    ///
    /// This is automatically called when registering DefmtSink as the global
    /// logger to prevent infinite loops. You typically don't need to call this
    /// manually.
    #[cfg(feature = "std")]
    pub fn disable_internal_atomic() {
        internal::GEID_STORE_GLOBAL.set_disabled_atomic();
    }
}
