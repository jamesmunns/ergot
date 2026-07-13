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
/// alive must schedule a re-poll, not park silently.
///
/// bbqueue hands out only one read grant at a time, so the second `recv()` sees
/// `GrantInProgress`. The socket's waker is only woken by a producer commit —
/// *releasing* the outstanding grant does not wake it — so if the poll just parked,
/// dropping that grant would never wake this future: a silent, permanent stall (a
/// guaranteed self-deadlock if the same task holds the grant). The poll must
/// instead reschedule itself so it makes progress the moment the grant is dropped.
#[test]
fn borrow_recv_reschedules_while_grant_outstanding() {
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

    // Poll a second recv while the first grant is still outstanding.
    let woke_before = cw.0.load(Ordering::SeqCst);
    let mut recv2 = Box::pin(hdl.recv());
    let p = recv2.as_mut().poll(&mut cx);
    assert!(
        p.is_pending(),
        "recv must be pending while a grant is outstanding"
    );
    assert!(
        cw.0.load(Ordering::SeqCst) > woke_before,
        "recv() with an outstanding grant must reschedule itself, or dropping the \
         grant would never wake it"
    );

    drop(g1);
}
