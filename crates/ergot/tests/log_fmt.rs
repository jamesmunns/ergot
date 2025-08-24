#[cfg(not(miri))]
#[cfg(feature = "std")]
#[tokio::test]
async fn fmt_log_pun() {
    use std::{pin::pin, time::Duration};

    use ergot::{
        NetStack, fmt, fmtlog::Level, interface_manager::profiles::null::Null,
        well_known::ErgotFmtRxTopic,
    };
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::time::timeout;

    type TestNetStack = NetStack<CriticalSectionRawMutex, Null>;
    let _ = env_logger::try_init();
    static STACK: TestNetStack = TestNetStack::new();

    // This doesn't work because SENDING uses send_bor, which owned sockets
    // don't support.
    // let logsub = STACK.std_bounded_topic_receiver::<ErgotFmtRxOwnedTopic>(16, None);
    // let mut logsub = pin!(logsub);
    // let mut sub = logsub.subscribe();

    let borq = STACK
        .topics()
        .heap_borrowed_topic_receiver::<ErgotFmtRxTopic>(1024, None, 256);
    let borq = pin!(borq);
    let mut sub = borq.subscribe();

    let x = 10;
    let msg = "world";

    STACK.trace_fmt(fmt!("1 hello ({x}), {}", msg));
    STACK.debug_fmt(fmt!("2 hello ({x}), {}", msg));
    STACK.info_fmt(fmt!("3 hello ({x}), {}", msg));
    STACK.warn_fmt(fmt!("4 hello ({x}), {}", msg));
    STACK.error_fmt(fmt!("5 hello ({x}), {}", msg));

    let levels = &[
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
    ];

    for _l in levels {
        let m = timeout(Duration::from_secs(1), sub.recv()).await.unwrap();
        let acc = m.try_access().unwrap().unwrap();
        println!("{} {:?}", acc.t.inner, acc.t.level);
    }
}
