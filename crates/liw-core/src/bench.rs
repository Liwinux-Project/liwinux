//! Frame timing measurement and analysis.
//!
//! # Methodology note
//!
//! SurfaceFlinger keeps a ROLLING buffer of 128 frames. That creates two traps
//! and the bash prototype fell into both:
//!
//! 1. **Sampling interval.** Longer than the buffer's fill time and frames are
//!    lost. At 180 Hz, 128 frames fill in 0.71 s; a fixed 1 s interval misses
//!    frames. The interval must be DERIVED from the refresh rate.
//! 2. **Window boundary artefact.** The gap between two snapshots is NOT a real
//!    frame interval. Intervals must be computed only from consecutive frames
//!    within the SAME snapshot; otherwise you get invented values like "worst
//!    500 ms".

use std::collections::BTreeSet;

/// Invalid timestamp bound. SurfaceFlinger writes a huge sentinel for frames
/// 0 veya INT64_MAX yazar.
const INVALID_MAX: i64 = i64::MAX / 2;

/// A single `--latency` snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Display refresh period (ns). The first line of the output.
    pub refresh_ns: u64,
    /// Valid present timestamps (ns), ascending, deduplicated.
    pub presents: Vec<i64>,
}

/// Parses `dumpsys SurfaceFlinger --latency <layer>` output.
///
/// Format: first line the refresh period, then three-column rows
/// (desiredPresentTime, actualPresentTime, frameReadyTime).
pub fn parse_latency(raw: &str) -> Option<Snapshot> {
    let mut lines = raw.lines().map(str::trim).filter(|l| !l.is_empty());
    let refresh_ns: u64 = lines.next()?.parse().ok()?;
    // Absurd refresh values are a parse error; do not accept them.
    if !(1_000_000..=100_000_000).contains(&refresh_ns) { return None; }

    let mut presents = BTreeSet::new();
    for l in lines {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() != 3 { continue; }
        let Ok(actual) = cols[1].parse::<i64>() else { continue };
        if actual <= 0 || actual > INVALID_MAX { continue; }
        presents.insert(actual);
    }
    Some(Snapshot { refresh_ns, presents: presents.into_iter().collect() })
}

/// Extracts frame intervals from the collected snapshots.
#[derive(Debug, Default)]
pub struct FrameData {
    /// Intervals (ms), deduplicated BY START FRAME.
    ///
    /// Snapshots overlap: dumpsys returns a 128-frame buffer but only ~57 new
    /// frames appear per sample, so the same interval shows up in 2-3 samples.
    /// Using a Vec counted it 2-3 times; a real measurement of 7997 unique
    /// frames produced 23560 "intervals".
    ///
    /// Percentiles are barely affected (numerator and denominator inflate
    /// together) but jank COUNTS become wrong and the sample size looks more
    /// trustworthy than it is. That matters for before/after comparisons.
    intervals: std::collections::BTreeMap<i64, f64>,
    /// Unique frames seen; used for the coverage calculation.
    frames: BTreeSet<i64>,
    /// Gaps beyond 1 s: FREEZES (menu loading, ANR, network timeout).
    ///
    /// Kept out of the percentiles — a single 60-second freeze would make p99
    /// meaningless. But they must not be discarded either: "I wait a minute in
    /// the ESC menu" is exactly this.
    stalls: std::collections::BTreeMap<i64, f64>,
    refresh_ns: u64,
}

impl FrameData {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, snap: &Snapshot) {
        if snap.refresh_ns > 0 { self.refresh_ns = snap.refresh_ns; }
        self.frames.extend(snap.presents.iter().copied());
        // Intervals come from WITHIN a window; boundary gaps are never used.
        for w in snap.presents.windows(2) {
            let d = (w[1] - w[0]) as f64 / 1e6;
            // Reject absurd values: below 0.05 ms is measurement noise, above
            // 1000 ms is a pause (the app may have gone to the background).
            if (0.05..1000.0).contains(&d) {
                // Keyed by the start frame: the same interval is counted once
                // no matter how many samples it appears in.
                self.intervals.insert(w[0], d);
            } else if d >= 1000.0 && d < 600_000.0 {
                self.stalls.insert(w[0], d);
            }
        }
    }

    pub fn interval_count(&self) -> usize { self.intervals.len() }

    /// Timestamped intervals: (monotonic ms, duration ms).
    ///
    /// Mandatory for diagnosis: "p99 12 ms" does not say WHEN a stutter
    /// happened, so it cannot be correlated with what was going on.
    pub fn intervals_ms(&self) -> Vec<(f64, f64)> {
        self.intervals.iter().map(|(t, d)| (*t as f64 / 1e6, *d)).collect()
    }

    /// Freezes beyond 1 s: (monotonic ms, duration ms).
    pub fn stalls_ms(&self) -> Vec<(f64, f64)> {
        self.stalls.iter().map(|(t, d)| (*t as f64 / 1e6, *d)).collect()
    }

    /// Time of the last frame seen (monotonic ms). Used to tell whether new
    /// frames are still arriving.
    pub fn last_frame_ms(&self) -> Option<f64> {
        self.frames.iter().next_back().map(|t| *t as f64 / 1e6)
    }
    pub fn frame_count(&self) -> usize { self.frames.len() }
    pub fn refresh_ms(&self) -> f64 { self.refresh_ns as f64 / 1e6 }

    /// Capture coverage: frames seen / frames expected.
    ///
    /// Low coverage means the numbers are not representative — reporting it is
    /// mandatory, otherwise a conclusion drawn from partial data looks solid.
    pub fn coverage_pct(&self) -> f64 {
        if self.frames.len() < 2 || self.refresh_ns == 0 { return 0.0; }
        let first = *self.frames.iter().next().unwrap();
        let last = *self.frames.iter().next_back().unwrap();
        let span_ns = (last - first) as f64;
        // Expected frame count follows the GAME's cadence. Computing it from
        // the refresh rate capped coverage at 33% for a game drawing 60 FPS on
        // a 180 Hz display — even with no frame missed at all.
        let period_ns = self.target_period_ms() * 1e6;
        if period_ns <= 0.0 { return 0.0; }
        let expected = span_ns / period_ns;
        if expected <= 0.0 { return 0.0; }
        (100.0 * self.frames.len() as f64 / expected).min(100.0)
    }

    /// Sorted intervals (for percentile computation).
    fn sorted(&self) -> Vec<f64> {
        let mut v: Vec<f64> = self.intervals.values().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    }

    /// The game's REAL frame period (ms) — NOT the display refresh period.
    ///
    /// Games are not required to draw at the display refresh rate. SFG2 locks to
    /// 60 FPS on a 180 Hz display: every frame lasts exactly 3 vsyncs. Measuring
    /// jank against the refresh period counted EVERY frame of that game as a
    /// stutter — 99.8% jank was reported while the game was perfectly regular.
    ///
    /// The median is used: measuring against the game's own target asks "how
    /// much longer did this frame take than a typical one", which is what jank
    /// means in the first place.
    pub fn target_period_ms(&self) -> f64 {
        let p50 = self.percentile(50.0);
        if p50 > 0.0 { p50 } else { self.refresh_ms() }
    }

    /// The FPS the game appears to be locked to.
    pub fn target_fps(&self) -> f64 {
        let t = self.target_period_ms();
        if t > 0.0 { 1000.0 / t } else { 0.0 }
    }

    /// Is the game drawing below the display refresh rate?
    ///
    /// If so, refresh-based coverage and jank figures are misleading; reporting
    /// this is mandatory.
    pub fn is_below_refresh(&self) -> bool {
        let r = self.refresh_ms();
        r > 0.0 && self.target_period_ms() > r * 1.5
    }

    pub fn percentile(&self, p: f64) -> f64 {
        let v = self.sorted();
        if v.is_empty() { return 0.0; }
        let i = ((v.len() as f64 - 1.0) * p / 100.0).round() as usize;
        v[i.min(v.len() - 1)]
    }

    pub fn mean_ms(&self) -> f64 {
        if self.intervals.is_empty() { return 0.0; }
        self.intervals.values().sum::<f64>() / self.intervals.len() as f64
    }

    /// Intervals longer than `mult` times the period (jank).
    /// Jank count, measured against the GAME's own frame period.
    pub fn jank_count(&self, mult: f64) -> usize {
        let t = self.target_period_ms() * mult;
        if t <= 0.0 { return 0; }
        self.intervals.values().filter(|&&x| x > t).count()
    }

    pub fn jank_pct(&self, mult: f64) -> f64 {
        if self.intervals.is_empty() { return 0.0; }
        100.0 * self.jank_count(mult) as f64 / self.intervals.len() as f64
    }
}

/// Derives the sampling interval from the refresh rate.
///
/// The buffer holds 128 frames; we sample before it fills, with a safety
/// margin. A fixed value loses frames at high refresh rates.
pub fn sample_interval_ms(refresh_ns: u64) -> u64 {
    const BUFFER_FRAMES: f64 = 128.0;
    const SAFETY: f64 = 0.45;
    let refresh_ms = refresh_ns as f64 / 1e6;
    let v = BUFFER_FRAMES * refresh_ms * SAFETY;
    v.clamp(80.0, 1000.0) as u64
}

#[cfg(test)]
mod stall_tests {
    use super::*;

    fn snap(refresh_ns: u64, presents: &[i64]) -> Snapshot {
        Snapshot { refresh_ns, presents: presents.to_vec() }
    }

    /// A freeze must NOT pollute the percentiles, but must NOT be lost either.
    ///
    /// A single 60-second menu freeze makes p99 meaningless; discarding it makes
    /// the user's actual complaint invisible.
    #[test]
    fn stalls_are_kept_apart_from_intervals() {
        let mut f = FrameData::new();
        // 3 normal frames (16 ms), then a 5 s gap, then one more frame.
        f.add(&snap(16_666_666, &[0, 16_000_000, 32_000_000,
                                  5_032_000_000, 5_048_000_000]));
        assert_eq!(f.interval_count(), 3, "the freeze must not enter the intervals");
        let st = f.stalls_ms();
        assert_eq!(st.len(), 1);
        assert!((st[0].1 - 5000.0).abs() < 1.0, "{st:?}");
        assert!((st[0].0 - 32.0).abs() < 0.1, "start of the freeze: {st:?}");
    }

    /// Intervals must carry timestamps; without them correlation is impossible.
    #[test]
    fn intervals_carry_their_timestamp() {
        let mut f = FrameData::new();
        f.add(&snap(16_666_666, &[1_000_000, 17_000_000, 50_000_000]));
        let iv = f.intervals_ms();
        assert_eq!(iv.len(), 2);
        assert!((iv[0].0 - 1.0).abs() < 1e-6, "{iv:?}");
        assert!((iv[0].1 - 16.0).abs() < 1e-6, "{iv:?}");
        assert!((iv[1].1 - 33.0).abs() < 1e-6, "{iv:?}");
        assert!((f.last_frame_ms().unwrap() - 50.0).abs() < 1e-6);
    }

    /// An absurdly long gap (10 min) must not even count as a freeze: the app
    /// went to the background, that is not a measurement.
    #[test]
    fn absurd_gaps_are_not_stalls() {
        let mut f = FrameData::new();
        f.add(&snap(16_666_666, &[0, 700_000_000_000]));
        assert!(f.stalls_ms().is_empty());
    }
}

#[cfg(test)]
mod tests {
    /// REAL MEASUREMENT: SFG2 locks to 60 FPS on a 180 Hz display.
    ///
    /// Measuring jank against the refresh period counted every frame as a
    /// stutter and reported 99.8%, while the game was perfectly regular.
    #[test]
    fn game_locked_below_refresh_is_not_all_jank() {
        let refresh = 5_555_555u64;          // 180 Hz
        let period = refresh as i64 * 3;     // oyun 60 FPS
        let mut fd = FrameData::new();
        fd.add(&Snapshot {
            refresh_ns: refresh,
            presents: (0..200).map(|i| i * period).collect(),
        });
        assert!((fd.target_period_ms() - 16.667).abs() < 0.01,
            "hedef periyot: {}", fd.target_period_ms());
        assert!((fd.target_fps() - 60.0).abs() < 0.1);
        assert!(fd.is_below_refresh(), "the game draws below the refresh rate");
        assert_eq!(fd.jank_pct(1.5), 0.0,
            "steady 60 FPS is NOT jank");
        assert!(fd.coverage_pct() > 99.0,
            "no frame was missed, coverage must be full: {}", fd.coverage_pct());
    }

    /// A real stutter must still be caught: a dropped frame in a 60 FPS stream.
    #[test]
    fn dropped_frame_at_60fps_is_still_jank() {
        let refresh = 5_555_555u64;
        let period = refresh as i64 * 3;
        let mut presents: Vec<i64> = (0..100).map(|i| i * period).collect();
        // Drop frame 50 -> an interval twice as long.
        presents.remove(50);
        let mut fd = FrameData::new();
        fd.add(&Snapshot { refresh_ns: refresh, presents });
        assert_eq!(fd.jank_count(1.5), 1, "a dropped frame must count as jank");
    }

    /// A game drawing at the refresh rate must still measure correctly
    #[test]
    fn game_at_full_refresh_still_measured_correctly() {
        let refresh = 5_555_555u64;
        let mut fd = FrameData::new();
        fd.add(&Snapshot {
            refresh_ns: refresh,
            presents: (0..200).map(|i| i * refresh as i64).collect(),
        });
        assert!(!fd.is_below_refresh());
        assert!((fd.target_fps() - 180.0).abs() < 1.0);
        assert_eq!(fd.jank_pct(1.5), 0.0);
    }

    /// Overlapping snapshots must not count the same interval MANY TIMES.
    ///
    /// dumpsys returns a 128-frame buffer but only ~57 new frames appear per
    /// sample; the same interval shows up in 2-3 of them. A real measurement of
    /// 7997 unique frames produced 23560 "intervals".
    #[test]
    fn overlapping_snapshots_do_not_inflate_sample_count() {
        let mut fd = super::FrameData::new();
        let r = 5_555_555u64;
        let mk = |base: i64, n: i64| super::Snapshot {
            refresh_ns: r,
            presents: (0..n).map(|i| base + i * r as i64).collect(),
        };
        // Three samples, each overlapping the previous one heavily.
        fd.add(&mk(0, 10));
        fd.add(&mk(5 * r as i64, 10));
        fd.add(&mk(10 * r as i64, 10));
        // Frames 0..20 -> 19 unique intervals. Inflated counting gave 27.
        assert_eq!(fd.frame_count(), 20);
        assert_eq!(fd.interval_count(), 19,
            "overlapping samples must not recount an interval");
    }

    /// Deduplication must not distort percentiles: all-equal intervals -> that p50.
    #[test]
    fn dedupe_preserves_percentiles() {
        let mut fd = super::FrameData::new();
        let r = 5_555_555u64;
        for k in 0..5i64 {
            fd.add(&super::Snapshot {
                refresh_ns: r,
                presents: (0..8).map(|i| (k * 4 + i) * r as i64).collect(),
            });
        }
        let p50 = fd.percentile(50.0);
        assert!((p50 - 5.5555).abs() < 0.01, "p50 = {p50}");
    }

    use super::*;

    const REAL: &str = "5555555\n\
        100000000 100000000 100000000\n\
        105555555 105555555 105555555\n\
        111111110 111111110 111111110\n";

    #[test]
    fn parses_refresh_and_presents() {
        let s = parse_latency(REAL).unwrap();
        assert_eq!(s.refresh_ns, 5_555_555);
        assert_eq!(s.presents.len(), 3);
    }

    #[test]
    fn rejects_invalid_timestamps() {
        let raw = format!("5555555\n0 0 0\n1 {} 1\n100 200 300\n", i64::MAX);
        let s = parse_latency(&raw).unwrap();
        assert_eq!(s.presents, vec![200], "0 ve INT64_MAX elenmeli");
    }

    #[test]
    fn rejects_absurd_refresh() {
        assert!(parse_latency("42\n100 200 300\n").is_none());
        assert!(parse_latency("999999999999\n100 200 300\n").is_none());
    }

    #[test]
    fn deduplicates_repeated_frames() {
        let raw = "5555555\n1 1000000 1\n2 1000000 2\n3 2000000 3\n";
        assert_eq!(parse_latency(raw).unwrap().presents, vec![1_000_000, 2_000_000]);
    }

    /// Window boundary artefact: the gap between two snapshots must not count
    /// as an interval. This is why the bash prototype reported "worst 500 ms".
    #[test]
    fn gap_between_snapshots_is_never_an_interval() {
        let mut fd = FrameData::new();
        // Two snapshots with a 1 second gap between them.
        fd.add(&Snapshot { refresh_ns: 5_555_555,
            presents: vec![0, 5_555_555, 11_111_110] });
        fd.add(&Snapshot { refresh_ns: 5_555_555,
            presents: vec![1_011_111_110, 1_016_666_665] });
        assert_eq!(fd.interval_count(), 3, "2+1 within-window intervals expected");
        let worst = fd.percentile(100.0);
        assert!(worst < 10.0, "a boundary gap must not count, worst={worst}");
    }

    #[test]
    fn percentiles_are_ordered() {
        let mut fd = FrameData::new();
        let mut p = vec![0i64];
        for i in 1..200 { p.push(p[i - 1] + 5_555_555); }
        fd.add(&Snapshot { refresh_ns: 5_555_555, presents: p });
        let (p50, p95, p99) = (fd.percentile(50.0), fd.percentile(95.0), fd.percentile(99.0));
        assert!(p50 <= p95 && p95 <= p99);
        assert!((p50 - 5.5555).abs() < 0.01, "p50 must be close to refresh: {p50}");
    }

    #[test]
    fn jank_counts_long_intervals() {
        let mut fd = FrameData::new();
        // Three normal frames, then one late frame.
        fd.add(&Snapshot { refresh_ns: 5_000_000, presents: vec![
            0, 5_000_000, 10_000_000, 30_000_000,
        ]});
        assert_eq!(fd.jank_count(1.5), 1, "a 20ms interval must be jank");
        assert!((fd.jank_pct(1.5) - 33.33).abs() < 0.1);
    }

    /// The sampling interval must be DERIVED from the refresh rate; a fixed
    /// value loses frames at high Hz.
    #[test]
    fn sample_interval_shrinks_at_high_refresh() {
        let at60 = sample_interval_ms(16_666_667);
        let at180 = sample_interval_ms(5_555_555);
        assert!(at180 < at60, "the interval must be shorter at 180Hz: {at180} vs {at60}");
        assert!(at180 >= 80, "the lower bound must hold");
        assert!(at60 <= 1000, "the upper bound must hold");
    }

    #[test]
    fn empty_data_is_safe() {
        let fd = FrameData::new();
        assert_eq!(fd.percentile(50.0), 0.0);
        assert_eq!(fd.mean_ms(), 0.0);
        assert_eq!(fd.coverage_pct(), 0.0);
        assert_eq!(fd.jank_count(1.5), 0);
    }

    #[test]
    fn coverage_reports_missing_frames() {
        let mut fd = FrameData::new();
        // Only 6 frames were seen across a span lasting 10 frame intervals.
        fd.add(&Snapshot { refresh_ns: 5_000_000, presents: vec![
            0, 5_000_000, 10_000_000, 15_000_000, 40_000_000, 50_000_000,
        ]});
        let c = fd.coverage_pct();
        assert!(c > 50.0 && c < 70.0, "coverage must be ~60%, {c}");
    }
}
