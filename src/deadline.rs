//! Bound host calls without relying on another plugin event to wake them.
pub async fn bounded<F: std::future::Future>(millis: u64, future: F) -> Result<F::Output, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut future = std::pin::pin!(future);
        let mut deadline = std::pin::pin!(crate::wasi::clocks::monotonic_clock::wait_for(
            millis * 1_000_000
        ));
        std::future::poll_fn(|cx| {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(cx) {
                return std::task::Poll::Ready(Ok(value));
            }
            if deadline.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(Err("连接请求超时，将自动重试".into()));
            }
            std::task::Poll::Pending
        })
        .await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = millis;
        Ok(future.await)
    }
}
