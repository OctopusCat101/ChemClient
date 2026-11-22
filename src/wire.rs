//! One host BLE write at a time, shared by management and music.
use std::sync::atomic::{AtomicBool, Ordering};

static BUSY: AtomicBool = AtomicBool::new(false);

pub struct Guard;
pub async fn acquire() -> Guard {
    loop {
        if BUSY
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Guard;
        }
        wait_for_retry().await;
    }
}

#[cfg(target_arch = "wasm32")]
async fn wait_for_retry() {
    // Different AstroBox callbacks run as separate component tasks. A Rust
    // waker alone cannot suspend or wake those tasks in this build.
    crate::wasi::clocks::monotonic_clock::wait_for(10_000_000).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn wait_for_retry() {
    let mut yielded = false;
    std::future::poll_fn(|context| {
        if std::mem::replace(&mut yielded, true) {
            std::task::Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}
impl Drop for Guard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
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
