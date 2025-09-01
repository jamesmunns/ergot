//! Format based logging
//!
//! This is a "good enough" log implementation to allow for basic debugging until
//! we have things like service discovery figured out, and can potentially do something
//! more efficient.
//!
//! This implementation relies on postcard-schema's type punning, and the fact that
//! [`Arguments`](core::fmt::Arguments) implements serialization by formatting to a
//! string. This allows us to send the output of `format_args!` to the netstack,
//! formatting directly into the outgoing packet buffer, up to the chosen MTU.
//!
//! Then, when receiving, we can choose to receive it either as a borrowed `&str`, or
//! as an owned `String`.

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// The log level of the message
#[derive(Serialize, Deserialize, Schema, Clone, Copy, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// A borrowed format message for sending
///
/// Type-punned with [`ErgotFmtRx`] and `ErgotFmtRxOwned` (with the `std` feature enabled).
#[derive(Serialize, Schema, Clone)]
pub struct ErgotFmtTx<'a> {
    pub level: Level,
    pub inner: &'a core::fmt::Arguments<'a>,
}

/// A borrowed format message for receiving
///
/// Type-punned with [`ErgotFmtTx`] and `ErgotFmtRxOwned` (with the `std` feature enabled).
#[derive(Serialize, Deserialize, Schema)]
pub struct ErgotFmtRx<'a> {
    pub level: Level,
    pub inner: &'a str,
}

/// An owned format message for receiving
///
/// Type-punned with [`ErgotFmtRx`] and [`ErgotFmtTx`]
#[cfg(feature = "std")]
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct ErgotFmtRxOwned {
    pub level: Level,
    pub inner: String,
}

#[macro_export]
macro_rules! fmt {
    ($fmt:expr) => {
        &::core::format_args!($fmt)
    };
    ($fmt:expr, $($toks: tt)*) => {
        &::core::format_args!($fmt, $($toks)*)
    };
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        traits::Topic,
        well_known::{ErgotFmtRxTopic, ErgotFmtTxTopic},
    };

    #[test]
    fn fmt_punning_works() {
        assert_eq!(ErgotFmtTxTopic::TOPIC_KEY, ErgotFmtRxTopic::TOPIC_KEY);
        #[cfg(feature = "std")]
        assert_eq!(
            crate::well_known::ErgotFmtRxOwnedTopic::TOPIC_KEY,
            ErgotFmtRxTopic::TOPIC_KEY
        );

        let x = 10;
        let y = "world";
        let res = postcard::to_vec::<_, 128>(&ErgotFmtTx {
            level: Level::Warn,
            inner: &format_args!("hello {x}, {}", y),
        })
        .unwrap();

        let res = postcard::from_bytes::<ErgotFmtRx<'_>>(&res).unwrap();
        assert_eq!(res.inner, "hello 10, world");
    }
}
