use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Schema, Clone, Copy)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Serialize, Schema, Clone)]
pub struct ErgotFmtTx<'a> {
    pub level: Level,
    pub inner: &'a core::fmt::Arguments<'a>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ErgotFmtRx<'a> {
    pub level: Level,
    pub inner: &'a str,
}

#[cfg(feature = "std")]
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct ErgotFmtRxOwned {
    pub level: Level,
    pub inner: String,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        traits::Topic,
        well_known::{ErgotFmtRxTopic, ErgotFmtRxOwnedTopic, ErgotFmtTxTopic},
    };

    fn taker(x: &ErgotFmtTx<'_>) -> Vec<u8> {
        postcard::to_stdvec(x).unwrap()
    }

    #[test]
    fn fmt_punning_works() {
        assert_eq!(ErgotFmtTxTopic::TOPIC_KEY, ErgotFmtRxTopic::TOPIC_KEY);
        assert_eq!(ErgotFmtRxOwnedTopic::TOPIC_KEY, ErgotFmtRxTopic::TOPIC_KEY);

        let x = 10;
        let y = "world";
        let res = taker(&ErgotFmtTx {
            level: Level::Warn,
            inner: &format_args!("hello {x}, {}", y),
        });

        let res = postcard::from_bytes::<ErgotFmtRx<'_>>(&res).unwrap();
        assert_eq!(res.inner, "hello 10, world");
    }
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
