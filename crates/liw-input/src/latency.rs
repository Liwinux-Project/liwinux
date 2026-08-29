//! Latency measurement.
//!
//! The full chain is:
//!
//! ```text
//! key -> kernel evdev -> [US] -> uinput -> libinput -> KWin
//!     -> wl_touch -> Waydroid -> Android input pipeline -> game
//! ```
//!
//! Only the `[US]` part is directly measurable: the difference between the
//! kernel timestamp of the evdev event and the moment we dispatch the touch.
//! The rest is invisible to this tool and **pretending otherwise would be
//! wrong** — so the report states plainly what it does and does not cover.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

/// How many samples the live window keeps.
///
/// The run-long percentiles answer "how did that session go"; a status bar
/// needs "how does it feel right now", and those are different questions. At
/// roughly 200 dispatches a second this is about ten seconds of history —
/// long enough to be stable, short enough that a stall shows up and then
/// clears instead of staining the number for the rest of the session.
const RECENT: usize = 2048;

#[derive(Debug, Default)]
pub struct LatencyStats {
    samples_us: Vec<u64>,
    /// Bounded tail of `samples_us`, for the live figure.
    recent: VecDeque<u64>,
    /// Events whose timestamp appears in the future rather than the past.
    /// Clock skew makes the measurement meaningless; we count these instead
    /// of silently treating them as zero.
    pub skipped: usize,
}

impl LatencyStats {
    pub fn new() -> Self { Self::default() }

    /// Records the time elapsed from the evdev event timestamp until now.
    pub fn record(&mut self, event_time: SystemTime) {
        match SystemTime::now().duration_since(event_time) {
            Ok(d) => self.push(d.as_micros() as u64),
            Err(_) => self.skipped += 1,
        }
    }

    pub fn record_duration(&mut self, d: Duration) {
        self.push(d.as_micros() as u64);
    }

    fn push(&mut self, us: u64) {
        self.samples_us.push(us);
        if self.recent.len() == RECENT { self.recent.pop_front(); }
        self.recent.push_back(us);
    }

    /// (p50, p99) over the live window, in microseconds.
    ///
    /// Separate from `percentiles` on purpose: mixing the two would let a
    /// single early stall sit in the status bar for the rest of the session.
    pub fn recent_percentiles(&self) -> (u64, u64) {
        if self.recent.is_empty() { return (0, 0) }
        let mut v: Vec<u64> = self.recent.iter().copied().collect();
        v.sort_unstable();
        (Self::pct(&v, 50.0), Self::pct(&v, 99.0))
    }

    pub fn recent_len(&self) -> usize { self.recent.len() }

    pub fn len(&self) -> usize { self.samples_us.len() }
    pub fn is_empty(&self) -> bool { self.samples_us.is_empty() }

    fn pct(sorted: &[u64], p: f64) -> u64 {
        if sorted.is_empty() { return 0; }
        let i = ((sorted.len() as f64 - 1.0) * p / 100.0).round() as usize;
        sorted[i.min(sorted.len() - 1)]
    }

    /// (p50, p95, p99, worst) in microseconds.
    pub fn percentiles(&self) -> (u64, u64, u64, u64) {
        let mut s = self.samples_us.clone();
        s.sort_unstable();
        (Self::pct(&s, 50.0), Self::pct(&s, 95.0), Self::pct(&s, 99.0),
         *s.last().unwrap_or(&0))
    }

    pub fn mean_us(&self) -> u64 {
        if self.samples_us.is_empty() { return 0; }
        self.samples_us.iter().sum::<u64>() / self.samples_us.len() as u64
    }

    pub fn report(&self, label: &str) -> String {
        if self.is_empty() {
            // Report the skipped count here too: if every sample was skipped
            // due to clock skew, saying "no samples" hides the real reason.
            return if self.skipped > 0 {
                format!("{label}: no usable samples                                 ({} skipped due to clock skew)", self.skipped)
            } else {
                format!("{label}: no samples")
            };
        }
        let (p50, p95, p99, max) = self.percentiles();
        let mut s = format!(
            "{label}  ({} samples)\n\
             \x20 p50 {:6.2} ms   p95 {:6.2} ms   p99 {:6.2} ms   worst {:6.2} ms   mean {:6.2} ms",
            self.len(),
            p50 as f64 / 1000.0, p95 as f64 / 1000.0, p99 as f64 / 1000.0,
            max as f64 / 1000.0, self.mean_us() as f64 / 1000.0);
        if self.skipped > 0 {
            s.push_str(&format!("\n  warning: {} samples skipped (clock skew)", self.skipped));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_known_set() {
        let mut l = LatencyStats::new();
        for us in 1..=100u64 { l.record_duration(Duration::from_micros(us * 100)); }
        let (p50, p95, p99, max) = l.percentiles();
        assert_eq!(max, 10_000);
        assert!(p50 > 0 && p50 < p95 && p95 <= p99 && p99 <= max);
    }

    /// The live window must forget. A stall at the start of a session
    /// should not still be in the status bar an hour later.
    #[test]
    fn the_recent_window_forgets() {
        let mut l = LatencyStats::new();
        // One terrible sample, then a long calm stretch that evicts it.
        l.record_duration(Duration::from_millis(900));
        for _ in 0..RECENT { l.record_duration(Duration::from_micros(500)); }
        let (p50, p99) = l.recent_percentiles();
        assert_eq!(p50, 500);
        assert_eq!(p99, 500, "the stall should have been evicted");
        assert_eq!(l.recent_len(), RECENT, "the window must stay bounded");

        // The run-long view still remembers it — that is its job.
        assert_eq!(l.percentiles().3, 900_000);
    }

    /// Before it fills, the window still has to answer.
    #[test]
    fn a_short_window_still_reports() {
        let mut l = LatencyStats::new();
        for us in [100u64, 200, 300] { l.record_duration(Duration::from_micros(us)); }
        let (p50, p99) = l.recent_percentiles();
        assert_eq!(p50, 200);
        assert_eq!(p99, 300);
    }

    #[test]
    fn an_empty_window_is_zero_not_a_panic() {
        assert_eq!(LatencyStats::new().recent_percentiles(), (0, 0));
    }

    #[test]
    fn empty_stats_are_safe() {
        let l = LatencyStats::new();
        assert_eq!(l.percentiles(), (0, 0, 0, 0));
        assert_eq!(l.mean_us(), 0);
        assert!(l.report("x").contains("no samples"));
    }

    /// A timestamp from the future must not be silently counted as zero.
    #[test]
    fn future_timestamps_are_counted_not_swallowed() {
        let mut l = LatencyStats::new();
        l.record(SystemTime::now() + Duration::from_secs(10));
        assert_eq!(l.len(), 0);
        assert_eq!(l.skipped, 1);
        assert!(l.report("x").contains("clock skew"));
    }

    #[test]
    fn single_sample_percentiles_are_that_sample() {
        let mut l = LatencyStats::new();
        l.record_duration(Duration::from_micros(2500));
        assert_eq!(l.percentiles(), (2500, 2500, 2500, 2500));
    }
}
