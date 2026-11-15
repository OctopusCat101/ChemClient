//! One host BLE write at a time, shared by management and music.
use std::{
    sync::{Mutex, OnceLock},
    task::{Poll, Waker},
};

#[derive(Default)]
struct Gate {
    busy: bool,
    waiters: Vec<Waker>,
}
static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
fn gate() -> &'static Mutex<Gate> {
    GATE.get_or_init(Default::default)
}
pub struct Guard;
pub async fn acquire() -> Guard {
    std::future::poll_fn(|cx| {
        let mut g = gate().lock().unwrap_or_else(|e| e.into_inner());
        if !g.busy {
            g.busy = true;
            Poll::Ready(Guard)
        } else {
            if !g.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                g.waiters.push(cx.waker().clone());
            }
            Poll::Pending
        }
    })
    .await
}
impl Drop for Guard {
    fn drop(&mut self) {
        let waiters = {
            let mut g = gate().lock().unwrap_or_else(|e| e.into_inner());
            g.busy = false;
            std::mem::take(&mut g.waiters)
        };
        for w in waiters {
            w.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Wake},
    };
    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }
    #[test]
    fn queued_send_waits_and_cancelled_waiter_cannot_lock_the_link() {
        let w = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&w);
        let mut first = Box::pin(acquire());
        let Poll::Ready(held) = first.as_mut().poll(&mut cx) else {
            panic!()
        };
        let mut cancelled = Box::pin(acquire());
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        drop(cancelled);
        let mut next = Box::pin(acquire());
        assert!(next.as_mut().poll(&mut cx).is_pending());
        drop(held);
        assert!(next.as_mut().poll(&mut cx).is_ready());
    }
}
