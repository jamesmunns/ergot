
use logger::logger;
pub use logger::set_logger_racy;

use crate::net_stack::NetStackHandle;

mod logger {
    use core::sync::atomic::Ordering;

    use log::Log;
    use portable_atomic::AtomicUsize;

    static NOP_LOGGER: NopLogger = NopLogger;
    static mut LOGGER: StaticLogger = StaticLogger::new();

    #[allow(static_mut_refs)]
    pub unsafe fn set_logger_racy(logger: &'static dyn Log) -> Result<(), ()> {
        unsafe { LOGGER.set_logger_racy(logger) }
    }

    #[allow(static_mut_refs)]
    pub fn logger() -> &'static dyn Log {
        unsafe { &mut LOGGER }.logger()
    }

    struct StaticLogger {
        logger: &'static dyn Log,
        state: AtomicUsize,
    }

    impl StaticLogger {
        pub const fn new() -> Self {
            Self {
                logger: &NOP_LOGGER,
                state: AtomicUsize::new(State::Uninitialized as usize),
            }
        }

        pub unsafe fn set_logger_racy(&mut self, logger: &'static dyn Log) -> Result<(), ()> {
            match unsafe {
                core::mem::transmute::<usize, State>(self.state.load(Ordering::Acquire))
            } {
                State::Initialized => Err(()),
                State::Uninitialized => {
                    self.logger = logger;
                    self.state
                        .store(State::Initialized as usize, Ordering::Release);
                    Ok(())
                }
                State::Initializing => {
                    unreachable!("set_logger_racy should not be used with other logging functions")
                }
            }
        }

        pub fn logger(&self) -> &'static dyn Log {
            if self.state.load(Ordering::Acquire) != State::Initialized as usize {
                &NOP_LOGGER
            } else {
                self.logger
            }
        }
    }

    #[repr(usize)]
    enum State {
        Uninitialized,
        Initializing,
        Initialized,
    }

    pub struct NopLogger;

    impl Log for NopLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            false
        }

        fn log(&self, _: &log::Record) {}
        fn flush(&self) {}
    }
}


pub struct LogSink<N: NetStackHandle + Send + Sync> {
    e_stack: N,
}

impl<N: NetStackHandle + Send + Sync> LogSink<N> {
    pub const fn new(e_stack: N) -> Self {
        Self { e_stack }
    }

    pub fn register_static(&'static self, level: log::LevelFilter) {
        #[cfg(not(feature = "std"))]
        critical_section::with(|_cs| unsafe {
            _ = log::set_logger_racy(self);
            log::set_max_level_racy(level);
        });
        #[cfg(feature = "std")]
        {
            _ = log::set_logger(self);
            log::set_max_level(level);
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
