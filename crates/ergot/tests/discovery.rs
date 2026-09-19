#[cfg(feature = "tokio-std")]
#[tokio::test]
async fn discovery_local() {
    use std::{sync::Arc, time::Duration};

    use ergot::{
        interface_manager::{ConstInit, profiles::null::Null},
        net_stack::ArcNetStack,
        well_known::DeviceInfo,
    };
    use maitake_sync::WaitQueue;
    use mutex::raw_impls::cs::CriticalSectionRawMutex;
    use tokio::{select, time::sleep};

    type NullStdStack = ArcNetStack<CriticalSectionRawMutex, Null>;

    let _ = env_logger::builder().is_test(true).try_init();
    let stack = NullStdStack::new_with_profile(Null::INIT);
    let stopper = Arc::new(WaitQueue::new());
    tokio::task::spawn({
        let stack = stack.clone();
        async move {
            let info = DeviceInfo {
                name: Some("testdisco".try_into().unwrap()),
                description: Some("I'm a test device!".try_into().unwrap()),
                unique_id: 1234,
            };
            let fut = stack.services().device_info_handler::<4>(&info);
            select! {
                _ = stopper.wait() => {}
                _ = fut => {}
            }
        }
    });
    sleep(Duration::from_millis(100)).await;
    let res = stack
        .discovery()
        .discover(4, Duration::from_millis(100))
        .await;
    assert_eq!(res.len(), 1);
    let msg = &res[0];
    assert_eq!(msg.info.name, Some("testdisco".try_into().unwrap()));
    assert_eq!(
        msg.info.description,
        Some("I'm a test device!".try_into().unwrap())
    );
    assert_eq!(msg.info.unique_id, 1234);
}
