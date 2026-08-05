//! Borrow-socket recv: the normal recv -> access -> drop -> recv flow.
//!
//! The `ResponseGrant` returned by `recv()` borrows the socket handle, so holding
//! one across another `recv()` on the same socket is a compile error (see the
//! `tests/ui` compile-fail case). This test exercises the correct flow.
#![cfg(feature = "std")]
#![cfg(not(miri))]

use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use ergot::{socket::Response, toolkits::null::new_arc_null_stack, topic};

topic!(StrTopic, String, "ergot/test/str");

struct NoopWaker;
impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

#[test]
fn borrow_recv_access_drop_then_recv_again() {
    let stack = new_arc_null_stack();
    let rx = stack
        .topics()
        .heap_bounded_borrowed_receiver::<StrTopic>(512, None, 128);
    let mut rx = pin!(rx);
    let mut hdl = rx.as_mut().subscribe();

    let waker = Waker::from(Arc::new(NoopWaker));
    let mut cx = Context::from_waker(&waker);

    // recv #1: a delivered message makes the first poll ready.
    stack
        .topics()
        .broadcast::<StrTopic>(&"one".to_string(), None)
        .unwrap();
    let g1 = {
        let mut fut = Box::pin(hdl.recv());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(g) => g,
            Poll::Pending => panic!("recv should be ready"),
        }
    };
    match g1.try_access().unwrap() {
        Response::Ok(m) => assert_eq!(m.t, "one"),
        Response::Err(_) => panic!("unexpected error frame"),
    }
    // The grant MUST be dropped before the next recv (enforced at compile time).
    drop(g1);

    // recv #2: the handle is free again.
    stack
        .topics()
        .broadcast::<StrTopic>(&"two".to_string(), None)
        .unwrap();
    let g2 = {
        let mut fut = Box::pin(hdl.recv());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(g) => g,
            Poll::Pending => panic!("recv should be ready"),
        }
    };
    match g2.try_access().unwrap() {
        Response::Ok(m) => assert_eq!(m.t, "two"),
        Response::Err(_) => panic!("unexpected error frame"),
    }
    drop(g2);
}
