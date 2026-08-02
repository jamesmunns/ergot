//! Regression test for borrow-socket recv while a response grant is outstanding.
#![cfg(feature = "std")]
// This is a scheduling/waker test, not a soundness test. It intentionally leaves a
// pending future holding a borrow, which trips Miri's leak checker on the null
// stack's queue Arc (unrelated to the behavior under test).
#![cfg(not(miri))]

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use ergot::{toolkits::null::new_arc_null_stack, topic};

topic!(StrTopic, String, "ergot/test/str");

struct CountingWaker(AtomicUsize);

impl Wake for CountingWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Polling `recv()` on a borrow socket while a previous `ResponseGrant` is still
/// alive must PARK, not busy-wake into a hot loop.
///
/// A borrow socket holds at most one outstanding read grant, so a `recv()` issued
/// while a grant is alive cannot make progress (dropping the grant does not wake
/// the socket waker — only a producer commit does). Holding a grant across a
/// `recv()` is a documented contract violation; the poll must simply stay parked
/// rather than re-waking itself every poll, which would spin at 100% CPU —
/// especially harmful on an embedded executor.
#[test]
fn borrow_recv_parks_while_grant_outstanding() {
    let stack = new_arc_null_stack();
    let rx = stack
        .topics()
        .heap_bounded_borrowed_receiver::<StrTopic>(512, None, 128);
    let mut rx = pin!(rx);
    let mut hdl = rx.as_mut().subscribe();

    // Deliver one message to the borrow socket.
    let sent = stack
        .topics()
        .broadcast::<StrTopic>(&"one".to_string(), None);
    assert_eq!(sent, Ok(()));

    let cw = Arc::new(CountingWaker(AtomicUsize::new(0)));
    let waker = Waker::from(cw.clone());
    let mut cx = Context::from_waker(&waker);

    // Receive the message and KEEP the grant alive.
    let g1 = {
        let mut recv1 = Box::pin(hdl.recv());
        match recv1.as_mut().poll(&mut cx) {
            Poll::Ready(g) => g,
            Poll::Pending => panic!("first recv should be ready"),
        }
    };

    // Poll a second recv repeatedly while the first grant is outstanding: it must
    // stay parked and must NOT wake itself (which would be a hot loop).
    let woke_before = cw.0.load(Ordering::SeqCst);
    let mut recv2 = Box::pin(hdl.recv());
    for _ in 0..5 {
        assert!(
            recv2.as_mut().poll(&mut cx).is_pending(),
            "recv must be pending while a grant is outstanding"
        );
    }
    assert_eq!(
        cw.0.load(Ordering::SeqCst),
        woke_before,
        "recv() with an outstanding grant must park, not busy-wake (hot loop)"
    );

    drop(g1);
}
