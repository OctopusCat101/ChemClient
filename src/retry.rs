use std::time::{Duration, Instant};

#[derive(Default)]
pub struct RetryClock(Option<Instant>);

impl RetryClock {
    pub fn reset(&mut self) {
        self.0 = None;
    }
    pub fn mark(&mut self) {
        self.0 = Some(Instant::now());
    }
    pub fn due(&self, interval: Duration) -> bool {
        self.0.is_none_or(|last| last.elapsed() >= interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopening_requests_a_probe_without_subtracting_from_the_clock() {
        let mut clock = RetryClock::default();
        assert!(clock.due(Duration::from_secs(16)));
        clock.mark();
        assert!(!clock.due(Duration::from_secs(16)));
        clock.reset();
        assert!(clock.due(Duration::from_secs(16)));
    }
}
