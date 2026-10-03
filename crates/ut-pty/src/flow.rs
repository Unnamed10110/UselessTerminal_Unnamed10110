use parking_lot::{Condvar, Mutex};
use std::time::Duration;

pub const HIGH_WATER: usize = 2 * 1024 * 1024;
pub const LOW_WATER: usize = 512 * 1024;

/// Credit window between the reader thread and the UI's acks (§3.5).
pub struct Flow {
    unacked: Mutex<usize>,
    cv: Condvar,
    high: usize,
    low: usize,
}

impl Flow {
    pub fn new(high: usize, low: usize) -> Self {
        Self { unacked: Mutex::new(0), cv: Condvar::new(), high, low: low.min(high) }
    }

    pub fn add(&self, n: usize) {
        *self.unacked.lock() += n;
    }

    pub fn ack(&self, n: usize) {
        let mut g = self.unacked.lock();
        *g = g.saturating_sub(n);
        if *g < self.low {
            self.cv.notify_all();
        }
    }

    pub fn unacked(&self) -> usize {
        *self.unacked.lock()
    }

    /// Reader side: if over HIGH, block until below LOW, `disposed()` or … forever re-checking every
    /// 250 ms (the timeout only guards against a lost wake-up). Blocking here stops draining the pipe,
    /// so ConPTY stops reading the child — the intended backpressure.
    pub fn wait_drained(&self, disposed: impl Fn() -> bool) {
        let mut g = self.unacked.lock();
        if *g <= self.high {
            return;
        }
        while *g >= self.low && !disposed() {
            self.cv.wait_for(&mut g, Duration::from_millis(250));
        }
    }

    pub fn wake(&self) {
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

    #[test]
    fn blocks_over_high_until_acked_below_low() {
        let f = Arc::new(Flow::new(100, 20));
        f.add(150);
        let done = Arc::new(AtomicBool::new(false));
        let (f2, d2) = (f.clone(), done.clone());
        let t = std::thread::spawn(move || {
            f2.wait_drained(|| false);
            d2.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(80));
        assert!(!done.load(Ordering::SeqCst), "must block while unacked > HIGH");
        f.ack(100); // 50 left: still ≥ LOW
        std::thread::sleep(Duration::from_millis(80));
        assert!(!done.load(Ordering::SeqCst));
        f.ack(40); // 10 < LOW
        t.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
    }

    #[test]
    fn not_blocked_under_high() {
        let f = Flow::new(100, 20);
        f.add(100);
        f.wait_drained(|| false);
    }
}
