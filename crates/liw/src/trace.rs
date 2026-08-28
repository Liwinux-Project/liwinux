//! `liw trace` — diagnosis that tells you WHY it stutters.
//!
//! `liw bench` says "how bad", `liw perf` says "which levers are on". Neither
//! answers "what caused this drop".
//!
//! The idea here is a single one: put everything on the SAME CLOCK. Frame
//! present times are `CLOCK_MONOTONIC`, so is `logcat -v monotonic`, and we
//! stamp host samples with it too. With a shared axis, "what was happening
//! during the stutter" becomes measurable.
//!
//! # Donma yakalama
//!
//! An FPS drop and a freeze are different things needing different methods. If
//! no frames arrive for 60 seconds there is no "long interval" — there are no
//! intervals at all. So the loop separately tracks "when did I last see a new
//! frame" and captures the log WHILE the freeze is happening. Looking afterwards
//! is usually too late: the logcat ring fills and drops the evidence.

use anyhow::{Context, Result};
use liw_core::bench::{parse_latency, sample_interval_ms, FrameData};
use liw_core::hostsample::{self, CpuMeter, HostSample};
use liw_core::trace::{self, Kind, LogEvent};
use liw_core::HelperClient;
use std::collections::HashSet;

/// How many ms without a frame before it counts as a freeze.
const STALL_MS: f64 = 900.0;

/// How many lines of log tail to pull.
///
/// Too small and events are missed between pulls; too large and the D-Bus
/// message bloats, slowing every pull.
const LOG_LINES: u32 = 400;

struct Stall {
    start_ms: f64,
    end_ms: Option<f64>,
    log: Vec<LogEvent>,
}

/// Accumulates log events without duplicates.
///
/// `logcat -t N` returns the last N lines on every pull, so consecutive pulls
/// OVERLAP heavily. Accumulating without deduplication counts the same event
/// dozens of times and wrecks the verdict entirely.
#[derive(Default)]
struct LogSink {
    seen: HashSet<(u64, u32, String)>,
    events: Vec<LogEvent>,
}

impl LogSink {
    fn add(&mut self, evs: Vec<LogEvent>) -> Vec<LogEvent> {
        let mut fresh = Vec::new();
        for e in evs {
            let key = ((e.t_ms * 1000.0) as u64, e.pid, e.msg.clone());
            if self.seen.insert(key) {
                fresh.push(e.clone());
                self.events.push(e);
            }
        }
        fresh
    }
}

/// Pulls and parses the log.
///
/// Tries the monotonic format first; falls back to wall-clock `Logcat` if the
/// helper is older. Returning empty silently would look like "no events at all"
/// and misdirect the diagnosis, so the caller is told which path was used.
async fn fetch_log(h: &HelperClient, monotonic: bool) -> (Vec<LogEvent>, bool) {
    let now = hostsample::monotonic_ms();
    let sod = hostsample::local_secs_of_day();
    if monotonic {
        if let Ok(raw) = h.log_trace("main", LOG_LINES).await {
            return (trace::parse_log(&raw, now, sod), true);
        }
    }
    match h.logcat("main", LOG_LINES).await {
        Ok(raw) => (trace::parse_log(&raw, now, sod), false),
        Err(_) => (Vec::new(), false),
    }
}

pub async fn run(pkg: String, duration_s: u64, jank_ms: Option<f64>) -> Result<()> {
    let h = HelperClient::connect().await
        .context("could not connect to liwd-helper — systemctl status liwd-helper")?;

    println!("Looking for a layer...");
    let layer = crate::bench::pick_layer(&h, &pkg).await?;
    let first = parse_latency(&h.surface_latency(&layer).await?)
        .context("could not parse the first snapshot")?;
    let interval = sample_interval_ms(first.refresh_ns);
    let refresh_ms = first.refresh_ns as f64 / 1e6;
    // A ROUGH threshold for the live display: the game's real cadence is only
    // known after measuring. The final report uses that.
    let jank_arg = jank_ms;
    let jank_ms = jank_arg.unwrap_or((refresh_ms * 2.0).max(20.0));

    // Is a monotonic log available? Try once and SAY so.
    let mono = h.log_trace("main", 1).await.is_ok();

    println!("Katman : {layer}");
    println!("Refresh: {refresh_ms:.2} ms ({:.1} Hz)  ->  sampling every {interval} ms",
        1000.0 / refresh_ms.max(0.001));
    if jank_arg.is_some() {
        println!("Thresholds: jank >{jank_ms:.1} ms   freeze >{:.0} ms", STALL_MS);
    } else {
        println!("Thresholds: freeze >{:.0} ms — the jank threshold will be \
                  derived from the measured cadence", STALL_MS);
    }
    println!("Log    : {}", if mono { "monotonic (fully aligned)" }
                            else { "wall clock (old helper — alignment approximate)" });
    println!("Length : {duration_s}s — DO THE THING THAT GOES WRONG");

    // Measure log noise: how much of the diagnostic window does it take?
    // The measurement must be UNFILTERED, or we cannot see what we silenced.
    let noise = measure_log_noise(&h).await;
    if let Some((tag, rate)) = noise.first() {
        if *rate > 50.0 {
            println!("Noise  : '{tag}' writes {rate:.0} lines/second                            — it narrows the diagnostic window");
        }
    }
    println!();

    let mut fd = FrameData::new();
    let mut host: Vec<(f64, HostSample)> = Vec::new();
    let mut cpu = CpuMeter::default();
    let mut sink = LogSink::default();
    let mut stalls: Vec<Stall> = Vec::new();

    let mut frame_tick = tokio::time::interval(
        std::time::Duration::from_millis(interval));
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut host_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut log_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut ui_tick = tokio::time::interval(std::time::Duration::from_secs(2));

    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(duration_s);
    // START of the trace: log events from before this must not enter the
    // report. `logcat -t N` returns the ring's last N lines, which on a quiet
    // system spans MINUTES; counting those told the lie "60 events in 20
    // seconds".
    let t_start_ms = hostsample::monotonic_ms();
    let mut last_frame_ms = hostsample::monotonic_ms();
    let mut last_seen: Option<f64> = None;
    let mut in_stall = false;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => { println!(); break; }

            _ = frame_tick.tick() => {
                let Ok(raw) = h.surface_latency(&layer).await else { continue };
                let Some(s) = parse_latency(&raw) else { continue };
                fd.add(&s);
                let now = hostsample::monotonic_ms();
                // "Did a new frame arrive?": whether the buffer's LAST frame advanced.
                if fd.last_frame_ms() != last_seen {
                    last_seen = fd.last_frame_ms();
                    last_frame_ms = now;
                    if in_stall {
                        in_stall = false;
                        if let Some(st) = stalls.last_mut() { st.end_ms = Some(now); }
                        let d = (now - stalls.last().map(|s| s.start_ms)
                                 .unwrap_or(now)) / 1000.0;
                        println!("  \x1b[32m✓ freeze ended\x1b[0m (lasted {d:.1} s)");
                    }
                } else if !in_stall && now - last_frame_ms > STALL_MS {
                    in_stall = true;
                    stalls.push(Stall { start_ms: last_frame_ms,
                                        end_ms: None, log: Vec::new() });
                    println!("  \x1b[33m⏸ FREEZE started\x1b[0m — capturing the log…");
                }
            }

            _ = host_tick.tick() => {
                host.push((hostsample::monotonic_ms(),
                           hostsample::sample(&mut cpu).await));
            }

            _ = log_tick.tick() => {
                let (evs, _) = fetch_log(&h, mono).await;
                let fresh = sink.add(evs);
                // Show and keep the evidence WHILE the freeze lasts: if the
                // logcat ring fills, looking later is too late.
                if in_stall {
                    if let Some(st) = stalls.last_mut() {
                        for e in &fresh {
                            println!("      \x1b[2m{}\x1b[0m {}: {}",
                                     e.kind.label(), e.tag, e.msg);
                        }
                        st.log.extend(fresh);
                    }
                }
            }

            _ = ui_tick.tick() => {
                if in_stall { continue; }
                let n = fd.interval_count();
                if n < 5 { continue; }
                let gpu = host.last().map(|(_, s)| s.gpu_pct).unwrap_or(0.0);
                let cpup = host.last().map(|(_, s)| s.cpu_pct).unwrap_or(0.0);
                print!("\r  {:5.1} FPS   p99 {:6.2} ms   jank {:3}   \
                        GPU {gpu:.0}%  CPU {cpup:.0}%   ({n} intervals)      ",
                    1000.0 / fd.percentile(50.0).max(0.001),
                    fd.percentile(99.0), fd.jank_count(1.5));
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }
    }
    println!();
    sink.events.retain(|e| e.t_ms >= t_start_ms - 250.0);
    report(&fd, &host, &sink, &stalls, jank_arg, mono, &noise);
    Ok(())
}

/// Measures log write rate per tag.
///
/// The DIFFERENCE between two unfiltered pulls is counted; a single pull only
/// says what is in the ring, not how fast it is filling.
async fn measure_log_noise(h: &HelperClient) -> Vec<(String, f64)> {
    let Ok(a) = h.logcat("main", 2000).await else { return Vec::new() };
    let t0 = hostsample::monotonic_ms();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let Ok(b) = h.logcat("main", 2000).await else { return Vec::new() };
    let span = (hostsample::monotonic_ms() - t0) / 1000.0;
    // Lines that are NEW in the second pull.
    let old: std::collections::HashSet<&str> = a.lines().collect();
    let fresh: String = b.lines().filter(|l| !old.contains(l))
        .collect::<Vec<_>>().join("\n");
    trace::tag_rates(&fresh, span)
}

fn report(fd: &FrameData, host: &[(f64, HostSample)], sink: &LogSink,
          stalls: &[Stall], jank_arg: Option<f64>, mono: bool, noise: &[(String, f64)]) {
    // The jank threshold is derived from the GAME's measured frame period.
    //
    // Deriving it from the refresh rate gave the wrong answer: on a game locked
    // to 60 FPS on a 180 Hz display, a 20 ms threshold counted the entirely
    // normal 22 ms frames as jank. The summary said 11 stutters while the
    // verdict said 450 — two different numbers from one run.
    let jank_ms = jank_arg.unwrap_or_else(|| (fd.target_period_ms() * 1.5).max(8.0));
    let line = "=".repeat(64);
    println!("{line}");

    if fd.interval_count() < 30 {
        println!("Not enough frame data ({} intervals).", fd.interval_count());
        println!("Was the game in the foreground and MOVING? A static screen produces no frames.");
        println!("{line}");
        return;
    }

    println!("FRAMES {} intervals, {} unique frames, coverage {:.0}%",
        fd.interval_count(), fd.frame_count(), fd.coverage_pct());
    if fd.is_below_refresh() {
        println!("       the game is locked to {:.0} FPS (display {:.0} Hz)",
            fd.target_fps(), 1000.0 / fd.refresh_ms().max(0.001));
    }
    println!("       jank threshold >{jank_ms:.1} ms (1.5x the game period of {:.2} ms)",
        fd.target_period_ms());
    println!("  p50 {:.2} ms ({:.0} FPS)   p99 {:.2} ms   worst {:.2} ms   \
              jank>1.5x %{:.2}",
        fd.percentile(50.0), 1000.0 / fd.percentile(50.0).max(0.001),
        fd.percentile(99.0), fd.percentile(100.0), fd.jank_pct(1.5));

    // --- donmalar ---
    let fd_stalls = fd.stalls_ms();
    if !stalls.is_empty() || !fd_stalls.is_empty() {
        println!();
        println!("DONMALAR");
        for s in stalls {
            let d = s.end_ms.map(|e| (e - s.start_ms) / 1000.0);
            match d {
                Some(d) => println!("  {d:.1} sn"),
                None => println!("  (still ongoing when the trace ended)"),
            }
            if s.log.is_empty() {
                println!("      No event at all on the Android side — the cause \
                          is most likely OUTSIDE the container.");
            }
            for e in s.log.iter().take(6) {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
        }
        for (t, d) in &fd_stalls {
            if stalls.iter().any(|s| (s.start_ms - t).abs() < 2000.0) { continue; }
            println!("  {:.1} sn (kare verisinden)", d / 1000.0);
            let ev: Vec<&LogEvent> = sink.events.iter()
                .filter(|e| e.t_ms >= t - 500.0 && e.t_ms <= t + d + 500.0)
                .collect();
            for e in ev.iter().take(4) {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
        }
    }

    // --- stutters and correlation ---
    let iv = fd.intervals_ms();
    let mut hs = trace::hitches(&iv, jank_ms);
    // The window must depend on the ACCURACY of the time alignment.
    //
    // With a monotonic log the alignment is exact and a narrow window matches
    // correctly. On wall clock the error reaches ±1 s and a narrow window
    // matches nothing — which made the tool say "unexplained" and suggest the
    // cause lay outside Android. This actually happened.
    let (before, after) = if mono { (150.0, 80.0) } else { (1500.0, 1500.0) };
    trace::correlate(&mut hs, &sink.events, before, after);
    hs.sort_by(|a, b| b.len_ms.partial_cmp(&a.len_ms).unwrap_or(std::cmp::Ordering::Equal));

    if !hs.is_empty() {
        println!();
        println!("EN UZUN TAKILMALAR");
        for hh in hs.iter().take(6) {
            println!("  {:.1} ms", hh.len_ms);
            if hh.evidence.is_empty() {
                println!("      (no concurrent event on the Android side)");
            }
            for e in &hh.evidence {
                println!("      {:<24} {}: {}", e.kind.label(), e.tag, e.msg);
            }
            // The host sample coinciding with that moment.
            if let Some((_, s)) = host.iter()
                .min_by(|a, b| (a.0 - hh.t_ms).abs()
                    .partial_cmp(&(b.0 - hh.t_ms).abs()).unwrap())
            {
                println!("      host: GPU {:.0}%  CPU {:.0}%  mem.pressure {:.2}",
                         s.gpu_pct, s.cpu_pct, s.mem_pressure);
            }
        }
    }

    // --- log signatures ---
    if !sink.events.is_empty() {
        let mut tally: std::collections::HashMap<Kind, usize> = Default::default();
        for e in &sink.events { *tally.entry(e.kind).or_default() += 1; }
        let mut v: Vec<_> = tally.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!();
        println!("LOG SIGNATURES ({} events, within the trace window)",
                 sink.events.len());
        for (k, n) in &v { println!("  {:<26} {n}", k.label()); }
        // Losing the input path deserves a warning of its own: injection dies
        // silently and the user experiences it as "the mouse does not work".
        if v.iter().any(|(k, _)| *k == Kind::Input) {
            println!();
            for l in wrap("WARNING: Android lost and re-created our touch device.                 When that happens the pipe handle we hold dies and injection                 stops. `liw keymap stop && liw keymap start --grab`                 brings it back.", 62) { println!("  {l}"); }
        }
    } else if !mono {
        println!();
        println!("No events came out of the log. Because the helper is old the \
                  time alignment is approximate;");
        println!("diagnosis sharpens noticeably after `sudo bash dist/install-helper.sh`.");
    }

    // --- host ---
    if !host.is_empty() {
        println!();
        println!("HOST ({} samples)", host.len());
        for (label, vals, unit) in [
            ("GPU", host.iter().map(|(_, h)| h.gpu_pct).collect::<Vec<_>>(), "%"),
            ("CPU", host.iter().map(|(_, h)| h.cpu_pct).collect(), "%"),
            ("mem.pressure", host.iter().map(|(_, h)| h.mem_pressure).collect(), ""),
        ] {
            let (mean, peak) = hostsample::summarise(&vals);
            println!("  {label:<10} mean {mean:7.1}{unit}   peak {peak:7.1}{unit}");
        }
        // Show VRAM WITH the total. A bare "4094 MB" reads as full; only the
        // ratio shows it is a third of 12288.
        let total = host.iter().map(|(_, h)| h.vram_total_mb).fold(0.0, f64::max);
        let (vmean, vpeak) = hostsample::summarise(
            &host.iter().map(|(_, h)| h.vram_mb).collect::<Vec<_>>());
        if total > 0.0 {
            println!("  {:<10} mean {vmean:7.0}MB   peak {vpeak:7.0}MB  / {total:.0}MB                       (peak {:.0}%)", "VRAM", 100.0 * vpeak / total);
        } else {
            println!("  {:<10} mean {vmean:7.0}MB   peak {vpeak:7.0}MB                        (total unreadable)", "VRAM");
        }
    }

    // --- log noise ---
    if let Some((tag, rate)) = noise.first() {
        if *rate > 50.0 {
            println!();
            println!("LOG NOISE");
            println!("  '{tag}' writes {rate:.0} lines per second.");
            for l in wrap(&format!(
                "This costs two things: the diagnostic tail covers milliseconds                  instead of seconds (events drop before they are seen), and                  every line is copied to logd. To silence it:                  waydroid prop set log.tag.{tag} S"), 62)
            { println!("  {l}"); }
        }
    }

    // --- verdict ---
    println!();
    println!("VERDICT");
    for v in trace::verdicts(&hs, fd.frame_count(), fd.jank_pct(1.5)) {
        println!("  ▸ {}", v.headline);
        for l in wrap(&v.detail, 62) { println!("    {l}"); }
    }

    // The host-side counterpart of stutters with no Android evidence.
    let mut facts = trace::HostFacts::default();
    let vram_total = host.iter().map(|(_, h)| h.vram_total_mb).fold(0.0, f64::max);
    for hh in hs.iter().filter(|h| h.evidence.is_empty()) {
        facts.unexplained += 1;
        let Some((_, s)) = host.iter().min_by(|a, b| (a.0 - hh.t_ms).abs()
            .partial_cmp(&(b.0 - hh.t_ms).abs()).unwrap()) else { continue };
        if s.gpu_pct >= 95.0 { facts.gpu_saturated += 1; }
        if vram_total > 0.0 && s.vram_mb / vram_total >= 0.92 {
            facts.vram_saturated += 1;
        }
        if s.mem_pressure >= 10.0 { facts.mem_pressed += 1; }
    }
    if let Some(v) = trace::host_verdict(&facts) {
        println!("  ▸ {}", v.headline);
        for l in wrap(&v.detail, 62) { println!("    {l}"); }
    } else if facts.unexplained > 0 {
        println!("  ▸ Host resources are not saturated either");
        for l in wrap("No event on the Android side and no saturation on the host.             That leaves the compositor/presentation path and power             management: look at `liw perf status`. If the log alignment is             approximate (old helper), evidence may also have been missed.", 62)
        { println!("    {l}"); }
    }
    if !mono {
        println!();
        println!("  NOTE: the helper is old — the log arrives on wall clock and");
        println!("  matching is ±1 s uncertain. After `sudo bash dist/install-helper.sh`");
        println!("  correlation drops to frame precision.");
    }
    println!("{line}");
}

/// Simple line wrapping; verdict text is long and must read in a terminal.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() { cur.push(' '); }
        cur.push_str(word);
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use liw_core::trace::Kind;

    /// Overlapping logcat pulls must count the same event ONCE.
    ///
    /// `logcat -t N` returns the last N lines on every call; accumulating
    /// without deduplication counts one event dozens of times and wrecks the
    #[test]
    fn overlapping_log_fetches_are_deduplicated() {
        let mut s = LogSink::default();
        let e = |t: f64| LogEvent { t_ms: t, pid: 7, tag: "art".into(),
            kind: Kind::Gc, msg: "GC freed".into() };
        assert_eq!(s.add(vec![e(1.0), e(2.0)]).len(), 2);
        // Second pull: one old line, one new.
        assert_eq!(s.add(vec![e(2.0), e(3.0)]).len(), 1);
        assert_eq!(s.events.len(), 3);
    }

    #[test]
    fn wrap_keeps_every_word_within_width() {
        let t = "short words in a long piece of text must wrap and no \
                 word may be lost";
        let ls = wrap(t, 20);
        assert!(ls.iter().all(|l| l.chars().count() <= 20), "{ls:?}");
        assert_eq!(ls.join(" ").split_whitespace().count(),
                   t.split_whitespace().count());
    }
}
