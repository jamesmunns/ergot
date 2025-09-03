//! This is a test to poke the existence of SocketPtrs and ensuring
//! that miri isn't upset about the whole thing.

use std::pin::pin;

use ergot::{NetStackSendError, toolkits::null::new_arc_null_stack, topic};

topic!(TestTopic, u64, "ergot/test");

#[test]
fn sockets() {
    let stack = new_arc_null_stack();

    let send_1 = stack.topics().broadcast_local::<TestTopic>(&123, None);
    assert_eq!(send_1, Err(NetStackSendError::NoRoute));

    let arc_skt_1 = Box::pin(stack.topics().heap_bounded_receiver::<TestTopic>(4, None));
    let mut arc_skt_1 = arc_skt_1.subscribe_boxed();

    // Make a scope, and a stack socket
    {
        let stk_skt_1 = stack.topics().bounded_receiver::<TestTopic, 4>(None);
        let stk_skt_1 = pin!(stk_skt_1);
        let mut stk_skt_1 = stk_skt_1.subscribe();
        let send_2 = stack.topics().broadcast_local::<TestTopic>(&1234, None);
        assert_eq!(send_2, Ok(()));
        assert_eq!(arc_skt_1.try_recv().unwrap().t, 1234);
        assert_eq!(stk_skt_1.try_recv().unwrap().t, 1234);
        // drop the stack socket
    }

    // Sending after dropping the stack item is fine
    let send_3 = stack.topics().broadcast_local::<TestTopic>(&12345, None);
    assert_eq!(send_3, Ok(()));
    assert_eq!(arc_skt_1.try_recv().unwrap().t, 12345);

    // Make a scope, and a stack socket
    let mut arc_skt_2 = {
        let stk_skt_2 = stack.topics().bounded_receiver::<TestTopic, 4>(None);
        let stk_skt_2 = pin!(stk_skt_2);
        let mut stk_skt_2 = stk_skt_2.subscribe();
        let send_4 = stack.topics().broadcast_local::<TestTopic>(&123456, None);
        assert_eq!(send_4, Ok(()));
        assert_eq!(arc_skt_1.try_recv().unwrap().t, 123456);
        assert_eq!(stk_skt_2.try_recv().unwrap().t, 123456);

        // drop the arc_skt
        drop(arc_skt_1);

        let send_5 = stack.topics().broadcast_local::<TestTopic>(&1234567, None);
        assert_eq!(send_5, Ok(()));
        assert_eq!(stk_skt_2.try_recv().unwrap().t, 1234567);

        // make a new arc skt
        let arc_skt_2 = Box::pin(stack.topics().heap_bounded_receiver::<TestTopic>(4, None));
        let mut arc_skt_2 = arc_skt_2.subscribe_boxed();

        let send_6 = stack.topics().broadcast_local::<TestTopic>(&12345678, None);
        assert_eq!(send_6, Ok(()));
        assert_eq!(arc_skt_2.try_recv().unwrap().t, 12345678);
        assert_eq!(stk_skt_2.try_recv().unwrap().t, 12345678);

        arc_skt_2
    };

    let send_7 = stack
        .topics()
        .broadcast_local::<TestTopic>(&123456789, None);
    assert_eq!(send_7, Ok(()));
    assert_eq!(arc_skt_2.try_recv().unwrap().t, 123456789);

    drop(arc_skt_2);

    let send_8 = stack
        .topics()
        .broadcast_local::<TestTopic>(&1234567890, None);
    assert_eq!(send_8, Err(NetStackSendError::NoRoute));

    // Okay, let's define + pin the socket in the outer scope
    let stk_skt_3 = stack.topics().bounded_receiver::<TestTopic, 4>(None);
    let mut stk_skt_3 = pin!(stk_skt_3);

    // New scope
    {
        let mut stk_skt_3sub1 = stk_skt_3.as_mut().subscribe();

        let send_9 = stack
            .topics()
            .broadcast_local::<TestTopic>(&12345678901, None);
        assert_eq!(send_9, Ok(()));
        assert_eq!(stk_skt_3sub1.try_recv().unwrap().t, 12345678901);

        // drop
    }

    let send_10 = stack
        .topics()
        .broadcast_local::<TestTopic>(&123456789012, None);
    assert_eq!(send_10, Err(NetStackSendError::NoRoute));

    // New scope
    {
        let mut stk_skt_3sub2 = stk_skt_3.as_mut().subscribe();

        let send_11 = stack
            .topics()
            .broadcast_local::<TestTopic>(&1234567890123, None);
        assert_eq!(send_11, Ok(()));
        assert_eq!(stk_skt_3sub2.try_recv().unwrap().t, 1234567890123);
        // drop
    }

    let send_12 = stack
        .topics()
        .broadcast_local::<TestTopic>(&123456789012, None);
    assert_eq!(send_12, Err(NetStackSendError::NoRoute));
}
