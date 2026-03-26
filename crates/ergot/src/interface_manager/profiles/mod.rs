//! Profiles describe how a single device interacts with the outside world
//!
//! Profiles have a couple of responsibilities, including managing all external interfaces,
//! as well as handling any routing outside of the device.

pub mod direct_edge;
pub mod null;

#[cfg(any(feature = "std", feature = "nostd-seed-router"))]
pub mod router;

// Backwards compatibility aliases
#[cfg(feature = "nostd-seed-router")]
pub mod no_std_router {
    //! Backwards compatibility — use [`super::router`] directly.
    pub use super::router::*;

    /// Backwards compatibility alias for [`super::router::Router`].
    pub type NoStdRouter<I, R, const N: usize, const S: usize> = super::router::Router<I, R, N, S>;
}

#[cfg(feature = "tokio-std")]
pub mod direct_router {
    //! Backwards compatibility — use [`super::router`] directly.
    pub use super::router::*;

    /// Backwards compatibility alias.
    ///
    /// Uses StdRng, 64 interface slots, 64 seed routes.
    pub type DirectRouter<I> = super::router::Router<I, rand::rngs::StdRng, 64, 64>;
}
