
use logger::logger;
pub use logger::set_logger_racy;

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
