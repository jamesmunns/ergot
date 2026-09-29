//! Time for ergot's timeouts, liveness and leases.
//!
//! One backend, picked by feature, serves the whole crate — and the
//! application, if it likes:
//!
//! | feature        | [`Instant::now`]                      | [`sleep`]                    |
//! |----------------|---------------------------------------|------------------------------|
//! | `tokio-std`    | tokio's clock (follows paused time)   | `tokio::time::sleep`         |
//! | `embassy-time` | embassy-time's driver                 | `embassy_time::Timer`        |
//! | `wasm`         | `performance.now()`                   | `setTimeout` (`gloo-timers`) |
//! | `std` alone    | `std::time::Instant`                  | —                            |
//!
//! Enable one backend. (Should several be on, e.g. under `--all-features`,
//! the first in the table wins.) Without a backend that can sleep, the parts
//! of ergot that wait — liveness, transmit and RPC timeouts, discovery — are
//! not available; the router only reads the time.
//!
//! Everything is milliseconds: every timeout and lease in ergot is
//! milliseconds to minutes. [`Instant`] is a `u64` of them, so it never
//! wraps and costs 8 bytes. The functions here are thin inline wrappers over
//! the backend's own.

#[cfg(time_sleep)]
use core::future::Future;
use core::ops::{Add, AddAssign};
pub use core::time::Duration;

#[cfg(time_sleep)]
use embassy_futures::select::{Either, select};

/// A point in time: milliseconds since an arbitrary, fixed epoch of the
/// backend (boot for embassy-time). Only differences between instants mean
/// anything.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Instant(u64);

impl Instant {
    /// The current time.
    #[cfg(time_now)]
    #[inline]
    pub fn now() -> Self {
        Self(backend::now_ms())
    }

    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Time from `earlier` to `self`, or zero if `earlier` is later.
    #[inline]
    pub const fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }

    /// Time since `self`.
    #[cfg(time_now)]
    #[inline]
    pub fn elapsed(self) -> Duration {
        Self::now().saturating_duration_since(self)
    }

    /// `self + duration`, or `None` on overflow.
    #[inline]
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(millis(duration)).map(Self)
    }
}

/// Saturates at the end of time rather than overflowing.
impl Add<Duration> for Instant {
    type Output = Instant;

    #[inline]
    fn add(self, duration: Duration) -> Instant {
        Instant(self.0.saturating_add(millis(duration)))
    }
}

impl AddAssign<Duration> for Instant {
    #[inline]
    fn add_assign(&mut self, duration: Duration) {
        *self = *self + duration;
    }
}

/// Whole milliseconds in `duration` (sub-millisecond parts are dropped).
#[inline]
fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

/// Wait for at least `duration`.
#[cfg(time_sleep)]
#[inline]
pub fn sleep(duration: Duration) -> impl Future<Output = ()> {
    backend::sleep(duration)
}

/// Wait until `deadline` (at once if it has passed).
#[cfg(time_sleep)]
#[inline]
pub fn sleep_until(deadline: Instant) -> impl Future<Output = ()> {
    sleep(deadline.saturating_duration_since(Instant::now()))
}

/// [`with_timeout`] ran out of time.
#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// Run `future` for at most `duration`.
#[cfg(time_sleep)]
pub async fn with_timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, TimedOut> {
    match select(future, sleep(duration)).await {
        Either::First(output) => Ok(output),
        Either::Second(()) => Err(TimedOut),
    }
}

#[cfg(feature = "tokio-std")]
mod backend {
    use std::sync::OnceLock;

    use tokio::time::{Instant, Sleep, sleep as tokio_sleep};

    use super::Duration;

    /// The first reading in the process.
    static EPOCH: OnceLock<Instant> = OnceLock::new();

    /// Where the epoch sits on the millisecond scale (~35 years in). A
    /// runtime with paused time runs its own clock, which can be behind the
    /// epoch — it starts at the real time the runtime was built, and moves
    /// only as its tasks sleep — so readings on either side of the epoch must
    /// stay ordered, not saturate to it.
    const EPOCH_MS: u64 = 1 << 40;

    #[inline]
    pub(super) fn now_ms() -> u64 {
        let now = Instant::now();
        let epoch = *EPOCH.get_or_init(|| now);
        match now.checked_duration_since(epoch) {
            Some(after) => EPOCH_MS.saturating_add(after.as_millis() as u64),
            None => EPOCH_MS.saturating_sub((epoch - now).as_millis() as u64),
        }
    }

    #[inline]
    pub(super) fn sleep(duration: Duration) -> Sleep {
        tokio_sleep(duration)
    }
}

#[cfg(all(not(feature = "tokio-std"), feature = "embassy-time"))]
mod backend {
    use embassy_time::{Instant, Timer};

    use super::Duration;

    #[inline]
    pub(super) fn now_ms() -> u64 {
        Instant::now().as_millis()
    }

    #[inline]
    pub(super) fn sleep(duration: Duration) -> Timer {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        Timer::after_micros(micros)
    }
}

#[cfg(all(
    not(feature = "tokio-std"),
    not(feature = "embassy-time"),
    feature = "std"
))]
mod backend {
    use std::sync::OnceLock;

    #[cfg(feature = "wasm")]
    use gloo_timers::future::TimeoutFuture;
    use web_time::Instant;

    #[cfg(feature = "wasm")]
    use super::Duration;

    /// `web-time` is `std::time` on native targets and `performance.now()`
    /// on wasm32, where `std::time::Instant::now()` panics.
    static EPOCH: OnceLock<Instant> = OnceLock::new();

    #[inline]
    pub(super) fn now_ms() -> u64 {
        let epoch = *EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_millis() as u64
    }

    #[cfg(feature = "wasm")]
    #[inline]
    pub(super) fn sleep(duration: Duration) -> TimeoutFuture {
        // `setTimeout` takes whole milliseconds; round up so the wait is never
        // shorter than asked. Beyond ~49 days it is clamped.
        let ms = duration.as_micros().div_ceil(1000).min(u32::MAX as u128) as u32;
        TimeoutFuture::new(ms)
    }
}

#[cfg(all(test, feature = "tokio-std"))]
mod tests {
    use std::time::Instant as WallInstant;

    use super::*;

    /// With paused time a sleep auto-advances the runtime clock; `now` must
    /// see that, or timeouts measured with it would never expire.
    #[tokio::test(start_paused = true)]
    async fn now_follows_paused_time() {
        let start = Instant::now();
        let wall = WallInstant::now();
        sleep(Duration::from_secs(60)).await;
        assert!(start.elapsed() >= Duration::from_secs(60));
        assert!(wall.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn with_timeout_gives_up_at_the_deadline() {
        let slow = sleep(Duration::from_secs(2));
        assert_eq!(
            with_timeout(Duration::from_secs(1), slow).await,
            Err(TimedOut)
        );
        let fast = sleep(Duration::from_millis(500));
        assert_eq!(with_timeout(Duration::from_secs(1), fast).await, Ok(()));
    }

    #[test]
    fn instant_arithmetic_saturates() {
        let t = Instant::from_millis(1_000);
        assert_eq!(
            t + Duration::from_millis(1_500),
            Instant::from_millis(2_500)
        );
        assert_eq!(
            t.saturating_duration_since(Instant::from_millis(2_000)),
            Duration::ZERO
        );
        assert_eq!(
            Instant::from_millis(u64::MAX - 1) + Duration::from_secs(1),
            Instant::from_millis(u64::MAX)
        );
        assert_eq!(
            Instant::from_millis(u64::MAX).checked_add(Duration::from_millis(1)),
            None
        );
    }
}
