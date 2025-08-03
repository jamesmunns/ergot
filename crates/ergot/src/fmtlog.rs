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

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        traits::Topic,
        well_known::{ErgotFmtRxTopic, ErgotFmtTxTopic},
    };

    fn taker(x: &ErgotFmtTx<'_>) -> Vec<u8> {
        postcard::to_stdvec(x).unwrap()
    }

    #[test]
    fn fmt_punning_works() {
        assert_eq!(ErgotFmtTxTopic::TOPIC_KEY, ErgotFmtRxTopic::TOPIC_KEY);

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
