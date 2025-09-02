// Okay, there are a couple things we have to think about here:
//
// * Which logging sink does ERGOT use for logging? Valid choices are:
//   * The global logger, as long as ERGOT is not that sink
//   * A user provided logger (hopefully not ergot)
//   * A null logger (nops)
// * Should ergot serve as the global logger sink?
//
// For Ergot's logger:
//
// * If !ERGOT_GLOBAL_SET && !ERGOT_SPECIFIC_SET: use `log::logger()`
// * If ERGOT_GLOBAL_SET && !ERGOT_SPECIFIC_SET: use NullLogger
// * If ERGOT_SPECIFIC_SET: use user logger, hope it isn't Ergot

use crate::net_stack::NetStackHandle;

pub(crate) mod internal {
    // TODO: for now!
    #![allow(dead_code)]

    use core::{
        cell::UnsafeCell,
        mem::MaybeUninit,
        sync::atomic::{AtomicU8, Ordering},
    };

    use critical_section::CriticalSection;

    pub(super) struct StaticLogger {
        manual_logger: UnsafeCell<MaybeUninit<&'static dyn log::Log>>,
        state: AtomicU8,
    }

    unsafe impl Sync for StaticLogger {}
    unsafe impl Send for StaticLogger {}

    impl StaticLogger {
        const fn new() -> Self {
            Self {
                manual_logger: UnsafeCell::new(MaybeUninit::uninit()),
                state: AtomicU8::new(GEIL_STATE_GLOBAL_DEFAULT),
            }
        }

        pub(super) fn set_null_with_cs(&'static self, _cs: CriticalSection<'_>) {
            // We only allow GLOBAL -> NULL to prevent going back and forth between
            // NULL and MANUAL
            if self.state.load(Ordering::Relaxed) != GEIL_STATE_GLOBAL_DEFAULT {
                return;
            }
            self.state.store(GEIL_STATE_NULL, Ordering::Relaxed);
        }

        #[cfg(target_has_atomic = "8")]
        pub(super) fn set_null_atomic(&'static self) {
            // We only allow GLOBAL -> NULL to prevent going back and forth between
            // NULL and MANUAL
            _ = self.state.compare_exchange(
                GEIL_STATE_GLOBAL_DEFAULT,
                GEIL_STATE_NULL,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
        }

        pub(super) fn set_manual_with_cs(
            &'static self,
            _cs: CriticalSection<'_>,
            dlog: &'static dyn log::Log,
        ) {
            // SAFETY: We only allowing writing of a manual handler IF we have not
            // ever written a manual handler before. This means we allow:
            //
            // * GLOBAL -> MANUAL
            // * NULL -> MANUAL
            //
            // We only write the state AFTER we have fully written the logger.
            //
            // This method can only be called with an active critical section token.
            unsafe {
                if self.state.load(Ordering::Relaxed) == GEIL_STATE_MANUAL {
                    return;
                }
                self.manual_logger.get().write(MaybeUninit::new(dlog));
                self.state.store(GEIL_STATE_MANUAL, Ordering::Relaxed);
            }
        }
    }

    impl Default for StaticLogger {
        fn default() -> Self {
            Self::new()
        }
    }

    pub(super) static GEIL_STORE_GLOBAL: StaticLogger = StaticLogger::new();
    const GEIL_STATE_GLOBAL_DEFAULT: u8 = 0;
    const GEIL_STATE_NULL: u8 = 1;
    const GEIL_STATE_MANUAL: u8 = 2;

    // "Get Ergot Internal Logger"
    //
    // TODO: We should use this in all internal logs, e.g. `log::debug!(geil(), "...");`
    #[inline]
    pub(crate) fn geil() -> &'static dyn log::Log {
        use core::sync::atomic::Ordering;

        match GEIL_STORE_GLOBAL.state.load(Ordering::Acquire) {
            GEIL_STATE_GLOBAL_DEFAULT => log::logger(),
            GEIL_STATE_MANUAL => {
                let ptr: *mut MaybeUninit<&'static dyn log::Log> =
                    GEIL_STORE_GLOBAL.manual_logger.get();
                // SAFETY: the global state is ONLY set to `GEIL_STATE_MANUAL` IF we have written
                // a valid `&'static dyn Log` into StaticLogger.manual_logger, and we ONLY allow
                // this once, which means we are allowed to perform unsynchronized reads of this
                // field, as we disallow transitions back from MANUAL->NULL or MANUAL->GLOBAL,
                // which means observing read tears is not possible.
                unsafe {
                    let muref: &'static MaybeUninit<&'static dyn log::Log> = &*ptr;
                    let dref: &'static dyn log::Log = muref.assume_init_ref();
                    dref
                }
            }
            _ => &NOP_LOGGER,
        }
    }

    pub(super) struct NopLogger;
    static NOP_LOGGER: NopLogger = NopLogger;

    impl log::Log for NopLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            false
        }

        fn log(&self, _: &log::Record) {}
        fn flush(&self) {}
    }
}

/// Set ergot's internal `log` sink
///
/// In order to allow `ergot` to act as a "sink" for user logs, e.g. sending
/// logs from an MCU to a server, ergot does not always use the global log sink.
///
/// This method can be used to manually set ergot's log sink to one of your choice.
///
/// This method is only effective ONCE, all further calls to this method will perform
/// no actions.
///
/// You SHOULD NOT ever use a [`LogSink`] as the given sink to this method. It will
/// not cause memory unsafety, but will likely cause an endless loop of ergot
/// "sending logs about sending logs about sending logs", essentially denial-of-service
/// attacking yourself.
pub fn set_ergot_internal_log_sink(sink: &'static dyn log::Log) {
    critical_section::with(|cs| {
        internal::GEIL_STORE_GLOBAL.set_manual_with_cs(cs, sink);
    })
}

pub struct LogSink<N: NetStackHandle + Send + Sync> {
    e_stack: N,
}

impl<N: NetStackHandle + Send + Sync> LogSink<N> {
    pub const fn new(e_stack: N) -> Self {
        Self { e_stack }
    }

    /// Register this LogSink as the global logger, with the given [`LevelFilter`](log::LevelFilter).
    ///
    /// If you do NOT set a manual logging sink with [`set_ergot_internal_log_sink()`], then this
    /// method has the effect of disabling all internal logging of ergot, to avoid a feedback loop.
    ///
    /// You may call this method and [`set_ergot_internal_log_sink()`] in any order.
    pub fn register_static(&'static self, level: log::LevelFilter) {
        #[cfg(not(feature = "std"))]
        critical_section::with(|cs| unsafe {
            _ = log::set_logger_racy(self);
            log::set_max_level_racy(level);
            internal::GEIL_STORE_GLOBAL.set_null_with_cs(cs);
        });
        #[cfg(feature = "std")]
        {
            _ = log::set_logger(self);
            log::set_max_level(level);
            internal::GEIL_STORE_GLOBAL.set_null_atomic();
        }
    }
}

impl<N: NetStackHandle + Send + Sync> log::Log for LogSink<N> {
    fn enabled(&self, _meta: &log::Metadata) -> bool {
        true
    }

    fn flush(&self) {}

    fn log(&self, record: &log::Record) {
        use log::Level::*;
        let stack = self.e_stack.stack();
        let args = record.args();
        match record.level() {
            Trace => stack.trace_fmt(args),
            Debug => stack.debug_fmt(args),
            Info => stack.info_fmt(args),
            Warn => stack.warn_fmt(args),
            Error => stack.error_fmt(args),
        }
    }
}
