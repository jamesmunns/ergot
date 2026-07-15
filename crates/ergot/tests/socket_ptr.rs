//! This is a test to poke the existence of SocketPtrs and ensuring
//! that miri isn't upset about the whole thing.

use std::pin::pin;

use ergot::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key, NetStackSendError,
    toolkits::null::new_arc_null_stack, topic, traits::Topic,
};

topic!(TestTopic, u64, "ergot/test");
topic!(StrTopic, String, "ergot/test/str");

/// An owned typed send delivered to a "borrow" socket must be serialized at the
/// sender's type, never reinterpreted as the socket's message type.
///
/// The borrow vtable used to expose a `recv_owned` that cast the sender's value
/// pointer straight to the socket's message type with no `TypeId` check (it cannot
/// use one — borrowed types pun across lifetimes). Delivering a `u64` to a borrow
/// socket expecting `String` (same topic FrameKind, matched by key) therefore
/// built a `&String` over the `u64`'s bytes and serialized it, reading a garbage
/// `(ptr, len)` — arbitrary memory access (UB, flagged by Miri). Owned sends to
/// borrow sockets now go through a serializer instantiated at the sender's type,
/// so the receiver simply fails to decode the bytes as its own type. Best observed
/// under Miri.
#[test]
fn owned_send_to_borrow_socket_is_type_safe() {
    let stack = new_arc_null_stack();
    let rx = stack
        .topics()
        .heap_bounded_borrowed_receiver::<StrTopic>(512, None, 128);
    let mut rx = pin!(rx);
    let _sub = rx.as_mut().subscribe();

    // Broadcast a u64 carrying StrTopic's key and the topic FrameKind, so it matches
    // the String borrow socket by key. A mismatched payload type must not be
    // reinterpreted as the socket's `String`.
    let hdr = Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 0,
            node_id: 0,
            port_id: 255,
        },
        any_all: Some(AnyAllAppendix {
            key: Key(<StrTopic as Topic>::TOPIC_KEY.to_bytes()),
            nash: None,
        }),
        seq_no: None,
        kind: FrameKind::TOPIC_MSG,
        ttl: DEFAULT_TTL,
    };
    let res = stack.send_ty::<u64>(&hdr, &0xDEAD_BEEF_1234_5678u64);
    // The send must be memory-safe. The bytes are serialized as a u64 and accepted
    // at the wire level; the receiver would simply fail to decode them as a String.
    assert_eq!(res, Ok(()), "unexpected send result: {res:?}");
}

/// A broadcast to a subscriber whose bounded queue is full must not be reported as
/// `NoRoute` — the audience exists, the message is just best-effort dropped for it.
/// `NoRoute` is reserved for "no audience at all".
#[test]
fn broadcast_to_full_subscriber_is_not_no_route() {
    let stack = new_arc_null_stack();
    // Single-slot subscriber.
    let rx = stack.topics().bounded_receiver::<TestTopic, 1>(None);
    let mut rx = pin!(rx);
    let _sub = rx.as_mut().subscribe();

    // Fill the one slot.
    assert_eq!(stack.topics().broadcast_local::<TestTopic>(&1, None), Ok(()));

    // Second broadcast: the subscriber is full, but it still exists.
    let res = stack.topics().broadcast_local::<TestTopic>(&2, None);
    assert_eq!(
        res,
        Ok(()),
        "a full subscriber is still an audience, not a missing route, got {res:?}"
    );
}

/// Sending a wrong-typed message to an owned socket of the right kind must return
/// a `TypeMismatch` error, not panic. It previously fired `debug_assert!(false, ..)`
/// on this path, which is reachable at runtime (e.g. a stale port after a peer
/// restart), so debug builds panicked instead of returning the error.
#[test]
fn owned_socket_type_mismatch_returns_error() {
    use ergot::socket::SocketSendError;

    let stack = new_arc_null_stack();
    // TestTopic carries a u64.
    let rx = stack.topics().bounded_receiver::<TestTopic, 4>(None);
    let mut rx = pin!(rx);
    let sub = rx.as_mut().subscribe_unicast();
    let port = sub.port();

    // Deliver a u32 to the u64 socket's exact port, with the same (topic) kind.
    let hdr = Header {
        src: Address::unknown(),
        dst: Address {
            network_id: 0,
            node_id: 0,
            port_id: port,
        },
        any_all: None,
        seq_no: None,
        kind: FrameKind::TOPIC_MSG,
        ttl: DEFAULT_TTL,
    };
    let res = stack.send_ty::<u32>(&hdr, &123u32);
    assert!(
        matches!(
            res,
            Err(NetStackSendError::SocketSend(SocketSendError::TypeMismatch))
        ),
        "expected a TypeMismatch error, got {res:?}"
    );
    drop(sub);
}

/// Re-subscribing a "borrow" socket after its handle was dropped must not corrupt
/// the intrusive socket list.
///
/// The borrow `SocketHdl` previously had no `Drop`, so dropping the handle did not
/// detach the socket from the netstack list. A second `subscribe()` on the same
/// pinned socket then re-inserted a still-linked node, double-linking it (an
/// `assert_ne!` in cordyceps `push_back`/`push_front`, or list UB). Raw/owned
/// sockets already handle this correctly (see the `sockets` test below); this
/// exercises the borrowed variant.
#[test]
fn borrow_reattach_after_handle_drop() {
    let stack = new_arc_null_stack();
    let rx = stack
        .topics()
        .heap_bounded_borrowed_receiver::<TestTopic>(4, None, 128);
    let mut rx = pin!(rx);

    // First subscribe, then drop the handle.
    let h1 = rx.as_mut().subscribe();
    drop(h1);

    // Re-subscribe the SAME pinned socket. Before the fix this double-linked the
    // node and panicked/corrupted the list.
    let h2 = rx.as_mut().subscribe();
    drop(h2);

    // And once more, for good measure.
    let h3 = rx.as_mut().subscribe();
    drop(h3);
}

/// Forgetting a socket handle must not leave a dangling node in the netstack list.
///
/// `raw_owned::Socket` previously had no `Drop`; unlinking happened only in the
/// handle's `Drop`. `mem::forget`-ing the handle (safe code) skipped that detach,
/// so when the pinned socket's own scope ended, its now-dangling node stayed in
/// the list — a use-after-free on the next send that walks the list. Best observed
/// under Miri (`cargo miri test --features std --test socket_ptr`).
#[test]
fn forgotten_handle_does_not_dangle() {
    let stack = new_arc_null_stack();

    {
        let rx = stack.topics().bounded_receiver::<TestTopic, 4>(None);
        let rx = pin!(rx);
        let sub = rx.subscribe();
        // Lose the handle without running its destructor.
        core::mem::forget(sub);
        // `rx` (the socket) is dropped at the end of this block. Its own Drop must
        // detach it, or its node dangles.
    }

    // Walk the socket list. Before the fix it still held a pointer to the freed
    // socket above (UAF); after the fix the backstop Drop detached it, so there is
    // simply no audience.
    let send = stack.topics().broadcast_local::<TestTopic>(&999, None);
    assert_eq!(send, Err(NetStackSendError::NoRoute));
}

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
