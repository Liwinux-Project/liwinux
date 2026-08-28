//! Stutter diagnosis: frame timing + Android log + host resources.
//!
//! # Why a separate module
//!
//! `bench` measures "how bad", `perf` looks at "which levers are on". Neither
//! answers **why**. The user's real questions are: "is this FPS drop coming
//! from the mouse system or from the game itself?" and "why does the ESC menu
//! take a minute to load?"
//!
//!
//! The answer only comes from CORRELATION: what was Android doing at the moment
//! of the stutter. This module does that matching.
//!
//! # Saf tutuluyor
//!
//! No I/O here; raw text goes in, findings come out. That makes it testable
//! against recorded logs without waiting for a real stutter — otherwise there
//! is no way to verify the diagnostic logic.

use serde::{Deserialize, Serialize};

/// What a log line tells us.
///
/// The ordering is not important but the distinction is: "a GC happened" and "a
/// network timeout occurred" are entirely different problems with different fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    /// The app's main thread did too much work (Choreographer/HWUI).
    MainThread,
    /// Garbage collection pause.
    Gc,
    /// Lock contention (monitor contention).
    Lock,
    /// The app is not responding.
    Anr,
    /// A binder call is slow or failing.
    Binder,
    /// Network: DNS did not resolve, connection timeout.
    Network,
    /// ARM bridge (libhoudini) or compilation/verification.
    ArmBridge,
    /// The compositor missed a frame.
    Composer,
    /// Generic "slow operation" warning.
    Slow,
    /// A process crashed / a tombstone was taken.
    Crash,
    /// The ad mediation stack is running (IronSource/Unity/Pangle/AdMob).
    AdStack,
    /// The game was PAUSED: another activity came to the front.
    ///
    /// An ad, a system dialog, a permission request... A paused app stops
    /// producing frames. The tool mistook this for a "freeze" and, on top of
    /// that, gave the WRONG verdict "the cause is outside the container".
    ///
    /// This actually happened: entering the menu showed an 8-second "freeze";
    /// the cause was the game's own ad activity (`gms.ads.AdActivity`).
    /// Nothing was wrong with the system.
    Paused,
    /// INPUT PATH: Android lost or re-found our input device.
    ///
    /// Exactly what the user asks: "is this stutter coming from the mouse
    /// system?" Only the input layer's own log can answer that.
    Input,
}

impl Kind {
    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Kind::MainThread => "app main thread",
            Kind::Gc => "garbage collection",
            Kind::Lock => "lock contention",
            Kind::Anr => "ANR (not responding)",
            Kind::Binder => "binder",
            Kind::Network => "network",
            Kind::ArmBridge => "ARM bridge / compilation",
            Kind::Composer => "compositor",
            Kind::Slow => "slow operation",
            Kind::Crash => "crash / tombstone",
            Kind::Paused => "game paused (another activity came forward)",
            Kind::AdStack => "ad mediation stack",
            Kind::Input => "INPUT PATH",
        }
    }
    /// How strongly this explains a stutter. The highest one leads the verdict.
    ///
    /// Network and ANR are highest: they can explain multi-second freezes ON
    /// THEIR OWN. GC is lowest: it happens constantly and is usually harmless.
    pub fn weight(self) -> u32 {
        match self {
            Kind::Anr => 100,
            // A broken input path is our own layer: it must appear at the
            // highest priority, because it is the only thing we can fix.
            Kind::Input => 95,
            Kind::Crash => 92,
            Kind::Network => 90,
            // Just above a pause: it says WHY the pause happened.
            Kind::AdStack => 89,
            // BELOW real faults.
            //
            // A pause EXPLAINS a frame gap but is not itself a fault; and it
            // can occur TOGETHER with a real one. Measured: the ad activity
            // opened in the menu produced both a pause and an ANR. Putting the
            // pause first would have hidden the ANR — the user would hear "no
            // problem" and miss the real error.
            Kind::Paused => 88,
            Kind::ArmBridge => 70,
            Kind::Lock => 60,
            Kind::Binder => 50,
            Kind::MainThread => 40,
            Kind::Slow => 30,
            Kind::Composer => 20,
            Kind::Gc => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    /// Monotonic time (ms). Same axis as the frame timestamps.
    pub t_ms: f64,
    pub pid: u32,
    pub tag: String,
    pub kind: Kind,
    /// Shortened message.
    pub msg: String,
}

/// Classifies one log line.
///
/// The patterns come from real Android output. A non-matching line returns
/// `None` and is IGNORED ENTIRELY: a diagnostic tool presenting noise as
/// evidence is worse than showing no evidence at all.
pub fn classify(tag: &str, msg: &str) -> Option<Kind> {
    let m = msg;
    // Order matters: a line can match several patterns and the most
    // explanatory one must win.
    if tag == "ActivityManager" && m.contains("ANR in") { return Some(Kind::Anr); }
    // Input path: if our touch pipe disappeared, injection dies SILENTLY.
    // Measured: on a display hotplug hwcomposer deletes and recreates the FIFO
    // and the handle we hold is orphaned.
    if tag == "EventHub" && (m.contains("wl_touch_events")
        || m.contains("wl_pointer_events") || m.contains("wayland_touch"))
    { return Some(Kind::Input); }
    // Game paused: no frames is NOT A FAULT.
    if tag == "InputDispatcher" && m.contains("because it is paused") {
        return Some(Kind::Paused);
    }
    if tag == "ActivityTaskManager" && m.contains("START u")
        && (m.contains("AdActivity") || m.contains("ads."))
    {
        return Some(Kind::Paused);
    }
    // Ad mediation stack: the most common cause of a stall in the menu.
    //
    // Measured: IronSource + UnityAds + Pangle + Google Ads are tried in turn,
    // the video ad is decoded IN SOFTWARE (OMX.google.vp9 / SoftAAC2 — Waydroid
    // has no hardware video decoder) and the game's main thread blocks long
    // enough to produce an ANR.
    if matches!(tag, "UnityAds" | "Ads" | "ironSourceSDK")
        || tag.ends_with("MediationAdapter")
    {
        return Some(Kind::AdStack);
    }
    if tag == "tombstoned" && m.contains("crash request") { return Some(Kind::Crash); }
    if m.contains("Fatal signal") || m.contains("Collecting stacks for native pid") {
        return Some(Kind::Crash);
    }
    if m.contains("Unable to resolve host")
        || m.contains("UnknownHostException")
        || m.contains("SocketTimeoutException")
        || m.contains("ConnectException")
        || m.contains("failed to connect to")
        || m.contains("Connection timed out")
    { return Some(Kind::Network); }
    if m.contains("Long monitor contention") { return Some(Kind::Lock); }
    if tag.starts_with("Choreographer") && m.contains("Skipped") {
        return Some(Kind::MainThread);
    }
    if m.starts_with("Davey!") { return Some(Kind::MainThread); }
    if tag == "houdini" || m.contains("houdini")
        || tag == "dex2oat" || m.contains("dex2oat")
        || m.contains("Verification of") || m.contains("Compilation of")
    { return Some(Kind::ArmBridge); }
    if m.contains("Slow Binder") || m.contains("binder thread pool")
        || m.contains("Binder transaction failed")
    { return Some(Kind::Binder); }
    if m.contains("Missed HW composer frame")
        || m.contains("GraphicBufferAllocator") && m.contains("failed")
    { return Some(Kind::Composer); }
    if m.contains("Slow operation") || m.contains("Slow Looper")
        || m.contains("Slow dispatch")
    { return Some(Kind::Slow); }
    // GC last: "GC freed" lines are very frequent and mostly harmless.
    if tag == "art" && m.contains("GC freed") { return Some(Kind::Gc); }
    None
}

/// Parses one logcat line.
///
/// TWO formats are supported:
///
/// * `-v monotonic -v threadtime` → `   1234.567  1234  1256 I Tag: msg`
///   Preferred: directly `CLOCK_MONOTONIC`, the same axis as frame timestamps,
///   with no timezone or year problem.
/// * default `threadtime` -> `08-28 00:48:47.391  88  88 I Tag: msg`
///   Fallback: only time of day, aligned using `now_ms`.
///
/// Both are needed because an older helper only produces the second one and the
/// diagnostic tool has to work with it too.
pub fn parse_line(line: &str, now_ms: f64, now_secs_of_day: f64) -> Option<LogEvent> {
    let l = line.trim_start();
    if l.is_empty() || l.starts_with("---") { return None; }
    let mut it = l.split_whitespace();
    let first = it.next()?;

    let (t_ms, mut it) = if first.contains('-') {
        // Wall-clock format: date, then time.
        let clock = it.next()?;
        let sod = secs_of_day(clock)?;
        // Wrap the day boundary: a log line appearing "after" us is yesterday.
        let mut age = now_secs_of_day - sod;
        if age < -1.0 { age += 86400.0; }
        (now_ms - age * 1000.0, it)
    } else {
        // Monotonic format: seconds directly.
        (first.parse::<f64>().ok()? * 1000.0, it)
    };

    let pid: u32 = it.next()?.parse().ok()?;
    let _tid = it.next()?;
    let _level = it.next()?;
    // The rest is "Tag: message".
    let rest = it.collect::<Vec<_>>().join(" ");
    let (tag, msg) = rest.split_once(':')?;
    let (tag, msg) = (tag.trim(), msg.trim());
    let kind = classify(tag, msg)?;
    Some(LogEvent {
        t_ms, pid, tag: tag.to_string(), kind,
        msg: truncate(msg, 160),
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    s.chars().take(n).collect::<String>() + "…"
}

/// `HH:MM:SS.mmm` -> seconds into the day.
fn secs_of_day(clock: &str) -> Option<f64> {
    let mut p = clock.split(':');
    let h: f64 = p.next()?.parse().ok()?;
    let m: f64 = p.next()?.parse().ok()?;
    let s: f64 = p.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

pub fn parse_log(raw: &str, now_ms: f64, now_secs_of_day: f64) -> Vec<LogEvent> {
    raw.lines()
        .filter_map(|l| parse_line(l, now_ms, now_secs_of_day))
        .collect()
}

/// Counts log lines per tag (noise measurement).
///
/// Why it is needed: Waydroid's hwcomposer writes two lines per frame. At
/// 180 Hz that is ~360 lines a second. This has two consequences and both
/// matter to the user:
///
/// 1. The diagnostic window collapses — a 400-line tail covers one second and
///    the events we are looking for drop out of the ring unseen.
/// 2. Every line is copied to `logd`; that is not free.
pub fn tag_rates(raw: &str, span_s: f64) -> Vec<(String, f64)> {
    let mut n: std::collections::HashMap<String, usize> = Default::default();
    for l in raw.lines() {
        let Some(tag) = tag_of(l) else { continue };
        *n.entry(tag).or_default() += 1;
    }
    let span = span_s.max(0.001);
    let mut v: Vec<(String, f64)> = n.into_iter()
        .map(|(t, c)| (t, c as f64 / span)).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// Extracts the tag from a line (format independent).
fn tag_of(line: &str) -> Option<String> {
    let l = line.trim_start();
    if l.is_empty() || l.starts_with("---") { return None; }
    let mut it = l.split_whitespace();
    let first = it.next()?;
    if first.contains('-') { it.next()?; }
    it.next()?; it.next()?; it.next()?;         // pid tid level
    let rest = it.collect::<Vec<_>>().join(" ");
    let (tag, _) = rest.split_once(':')?;
    Some(tag.trim().to_string())
}

/// A long frame interval and the evidence explaining it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hitch {
    /// The moment the stutter STARTED (ms, monotonic).
    pub t_ms: f64,
    /// Its duration (ms).
    pub len_ms: f64,
    pub evidence: Vec<LogEvent>,
}

/// Counts frame intervals above the threshold as stutters.
///
/// The threshold comes from outside because the right value depends on the
/// refresh rate: 33 ms is not jank at 60 Hz, it is at 180 Hz.
pub fn hitches(intervals: &[(f64, f64)], threshold_ms: f64) -> Vec<Hitch> {
    intervals.iter()
        .filter(|(_, d)| *d >= threshold_ms)
        .map(|(t, d)| Hitch { t_ms: *t, len_ms: *d, evidence: Vec::new() })
        .collect()
}

/// Attaches to each stutter the log events falling in its time window.
///
/// The window also looks BEFORE the stutter: the cause is usually logged just
/// before it (a GC starting, a network request), the effect afterwards.
pub fn correlate(hs: &mut [Hitch], events: &[LogEvent], before_ms: f64, after_ms: f64) {
    for h in hs.iter_mut() {
        let lo = h.t_ms - before_ms;
        let hi = h.t_ms + h.len_ms + after_ms;
        h.evidence = events.iter()
            .filter(|e| e.t_ms >= lo && e.t_ms <= hi)
            .cloned()
            .collect();
        // The most explanatory evidence first.
        h.evidence.sort_by(|a, b| b.kind.weight().cmp(&a.kind.weight()));
        h.evidence.truncate(4);
    }
}

/// One diagnostic result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: Option<Kind>,
    pub headline: String,
    pub detail: String,
}

/// Derives verdicts from stutters and events.
///
/// Rule: NO EVIDENCE, NO ACCUSATION. Saying "probably the GPU" is guessing
/// without measuring and sends the user to the wrong place. When no evidence is
/// found we say so plainly.
pub fn verdicts(hs: &[Hitch], total_frames: usize, jank_pct: f64) -> Vec<Verdict> {
    let mut out = Vec::new();
    if hs.is_empty() {
        out.push(Verdict {
            kind: None,
            headline: "No stutter captured".into(),
            detail: format!("{total_frames} frames traced, no interval above the \
                             threshold. Run it while you are experiencing the \
                             problem."),
        });
        return out;
    }

    // Group evidence kinds by STUTTER COUNT (not line count): 50 GC lines in a
    // single stutter does not make GC the prime suspect; GC appearing in 50
    // separate stutters does.
    let mut tally: std::collections::HashMap<Kind, usize> = Default::default();
    for h in hs {
        let mut seen: std::collections::HashSet<Kind> = Default::default();
        for e in &h.evidence { seen.insert(e.kind); }
        for k in seen { *tally.entry(k).or_default() += 1; }
    }

    let unexplained = hs.iter().filter(|h| h.evidence.is_empty()).count();
    let mut ranked: Vec<(Kind, usize)> = tally.into_iter().collect();
    ranked.sort_by(|a, b| (b.1 * b.0.weight() as usize)
        .cmp(&(a.1 * a.0.weight() as usize)));

    for (k, n) in ranked.iter().take(3) {
        let pct = 100.0 * *n as f64 / hs.len() as f64;
        out.push(Verdict {
            kind: Some(*k),
            headline: format!("{} — in {n}/{} stutters ({pct:.0}%)",
                              k.label(), hs.len()),
            detail: explain(*k).into(),
        });
    }
    if unexplained > 0 {
        out.push(Verdict {
            kind: None,
            headline: format!("{unexplained}/{} stutters UNEXPLAINED", hs.len()),
            detail: "No concurrent event was found on the Android side next to \
                     these stutters. Look at the host findings below."
                .into(),
        });
    }
    if jank_pct > 5.0 {
        out.push(Verdict {
            kind: None,
            headline: format!("Jank rate is high: {jank_pct:.1}%"),
            detail: "This is not the odd stutter but a constant problem. \
                     First lower the resolution/refresh rate and measure again: \
                     if it improves you are at the GPU limit.".into(),
        });
    }
    out
}

/// The host-side counterpart of unexplained stutters.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostFacts {
    /// In how many unexplained stutters the GPU was saturated.
    pub gpu_saturated: usize,
    /// In how many unexplained stutters VRAM was near full.
    pub vram_saturated: usize,
    /// In how many unexplained stutters memory pressure was high.
    pub mem_pressed: usize,
    pub unexplained: usize,
}

/// Looks at the host side when there is no evidence on the Android side.
///
/// Stopping at "unexplained" leaves the user with nothing; the host data we
/// already measure often holds the answer. The NO EVIDENCE, NO ACCUSATION rule
/// still applies: if no host signal was saturated it returns `None`.
pub fn host_verdict(f: &HostFacts) -> Option<Verdict> {
    if f.unexplained == 0 { return None; }
    let half = f.unexplained.div_ceil(2);
    if f.gpu_saturated >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "In {}/{} unexplained stutters the host GPU was saturated",
                f.gpu_saturated, f.unexplained),
            detail: "No event on the Android side, but the host GPU was at the                      ceiling in those moments. Lower the resolution or refresh                      rate and measure again; if stutters drop you are at the GPU limit."
                .into(),
        });
    }
    if f.vram_saturated >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "In {}/{} unexplained stutters VRAM was near full",
                f.vram_saturated, f.unexplained),
            detail: "When VRAM fills the driver swaps textures, which produces                      the occasional long frame. Close other GPU-hungry apps."
                .into(),
        });
    }
    if f.mem_pressed >= half {
        return Some(Verdict {
            kind: None,
            headline: format!(
                "In {}/{} unexplained stutters memory pressure was high",
                f.mem_pressed, f.unexplained),
            detail: "When host memory tightens, page reclaim produces waits.                      The `/proc/pressure/memory` measurement shows this."
                .into(),
        });
    }
    None
}

fn explain(k: Kind) -> &'static str {
    match k {
        Kind::AdStack =>
            "The ad mediation stack is running: the SDK tries several ad \
             networks in turn and decodes the video ad IN SOFTWARE — Waydroid \
             has no hardware video decoder. This can block the game's main \
             thread for seconds and go as far as an ANR. It is the app's own \
             behaviour, independent of our layer.",
        Kind::Paused =>
            "The game was PAUSED: another activity came to the front (an ad, a \
             system dialog, a permission request). A paused app produces no \
             frames, so that is the explanation for the frame gap. \
             CAREFUL: this alone does not mean 'nothing is wrong' — if an ANR \
             or a crash is also listed above, that is the real issue and the \
             pause is only its visible face.",
        Kind::Network =>
            "The game makes a network request and times out. This is the most \
             common reason for waiting minutes on a menu opening: when the \
             ad/analytics SDK cannot resolve DNS it waits out the socket \
             timeout (usually 30 s, two attempts). Verify DNS with `liw net doctor`.",
        Kind::Anr =>
            "The app did not respond. The ANR block in the log says which \
             thread was stuck where.",
        Kind::ArmBridge =>
            "ARM code is being translated to x86, or classes are being \
             verified. Happens on first launch and on first entry to a new \
             screen; normal if it does not recur the second time.",
        Kind::Lock =>
            "Two threads are waiting on the same lock. The app's own \
             problem; independent of our layer.",
        Kind::Binder =>
            "A system service call is slow. Usually means system_server is \
             under load.",
        Kind::MainThread =>
            "The game did more work on its main thread than the frame budget \
             allows. Our injection layer does not produce this — touch events \
             arrive on a separate path.",
        Kind::Slow =>
            "The system found its own operation slow. On its own it does not \
             point to a cause; read it together with the other evidence.",
        Kind::Composer =>
            "The compositor missed a frame. There may be GPU or compositor \
             congestion on the host side.",
        Kind::Input =>
            "Android lost or re-created our input device. When the touch pipe \
             is deleted and recreated (a display hotplug) the handle we hold \
             dies and injection stops silently. Restarting the keymapper \
             fixes it.",
        Kind::Crash =>
            "A process crashed or a tombstone was taken. Taking a dump can \
             last seconds and the whole system stutters meanwhile.",
        Kind::Gc =>
            "A garbage collection pause. It happens constantly and is usually \
             harmless; only be suspicious if it appears in every stutter.",
    }
}

#[cfg(test)]
mod tests {
    /// REAL LOG: the source of the menu stall was the ad mediation stack.
    #[test]
    fn ad_mediation_tags_are_recognised() {
        for (tag, msg) in [
            ("UnityAds", "Unity Ads was not able to get current network type"),
            ("IronSourceMediationAdapter", "Loading IronSource interstitial ad"),
            ("UnityMediationAdapter", "Unity Ads is initialized for game ID"),
            ("PangleMediationAdapter", "{"),
            ("ironSourceSDK", "API: f a - Interstitial The requested instance does not exist"),
            ("Ads", "canOpenAppGmsgHandler disabled."),
        ] {
            assert_eq!(super::classify(tag, msg), Some(super::Kind::AdStack),
                "{tag} must be recognised");
        }
    }

    /// The ad stack EXPLAINS the pause, but must not hide the ANR.
    #[test]
    fn ad_stack_ranks_between_pause_and_real_faults() {
        assert!(super::Kind::AdStack.weight() > super::Kind::Paused.weight());
        assert!(super::Kind::Anr.weight() > super::Kind::AdStack.weight());
    }

    /// A pause must NOT HIDE real faults.
    ///
    /// Measured: the ad activity in the menu produced both a pause and an ANR.
    /// Putting the pause first would have told the user "nothing is wrong".
    #[test]
    fn real_faults_outrank_a_pause() {
        for worse in [super::Kind::Anr, super::Kind::Crash,
                      super::Kind::Input, super::Kind::Network] {
            assert!(worse.weight() > super::Kind::Paused.weight(),
                "{worse:?} must come before a pause");
        }
    }

    /// But a pause must stay above the noise: if it is the only evidence, say it.
    #[test]
    fn pause_still_outranks_noise() {
        for lesser in [super::Kind::Composer, super::Kind::Gc,
                       super::Kind::Slow, super::Kind::MainThread] {
            assert!(super::Kind::Paused.weight() > lesser.weight());
        }
    }

    /// REAL LOG: entering the menu showed an 8-second "freeze".
    /// The cause was the game's own ad; nothing was wrong with the system.
    #[test]
    fn ad_activity_is_recognised_as_pause_not_fault() {
        assert_eq!(super::classify("ActivityTaskManager",
            "START u0 {cmp=com.ForgeGames.SpecialForcesGroup2/\
             com.google.android.gms.ads.AdActivity (has extras)} from uid 10148"),
            Some(super::Kind::Paused));
        assert_eq!(super::classify("InputDispatcher",
            "Not sending touch event to 1328819 com.ForgeGames.SpecialForcesGroup2/\
             com.epicgames.ue4.GameActivity because it is paused"),
            Some(super::Kind::Paused));
    }

    /// An ordinary activity start must NOT count as a pause.
    #[test]
    fn ordinary_activity_start_is_not_a_pause() {
        assert_ne!(super::classify("ActivityTaskManager",
            "START u0 {cmp=com.kiloo.subwaysurf/.MainActivity} from uid 10148"),
            Some(super::Kind::Paused));
    }

    use super::*;

    #[test]
    fn monotonic_line_parses() {
        let e = parse_line(
            "  1234.567890  4321  4350 I Choreographer: Skipped 42 frames!  \
             The application may be doing too much work on its main thread.",
            0.0, 0.0).unwrap();
        assert!((e.t_ms - 1_234_567.89).abs() < 0.1, "{}", e.t_ms);
        assert_eq!(e.pid, 4321);
        assert_eq!(e.kind, Kind::MainThread);
    }

    /// The wall-clock format must be understood too: an older helper produces
    /// only that and the tool has to work with it.
    #[test]
    fn wallclock_line_is_aligned_to_now() {
        // Now is 100.0 s into the day, monotonic 500_000 ms.
        // The line reads 00:01:30 = 90 s, i.e. 10 s ago.
        let e = parse_line(
            "08-28 00:01:30.000    88    88 W art: Long monitor contention with owner x",
            500_000.0, 100.0).unwrap();
        assert!((e.t_ms - 490_000.0).abs() < 1.0, "{}", e.t_ms);
        assert_eq!(e.kind, Kind::Lock);
    }

    /// A log line crossing midnight must not be taken as coming from the future.
    #[test]
    fn wallclock_wraps_over_midnight() {
        // Now 00:00:05 (5 s), the line 23:59:55 (86395 s) = 10 s ago.
        let e = parse_line(
            "08-28 23:59:55.000    88    88 W art: Long monitor contention with owner x",
            500_000.0, 5.0).unwrap();
        assert!((e.t_ms - 490_000.0).abs() < 1.0, "{}", e.t_ms);
    }

    /// Unrelated lines must be filtered out ENTIRELY. Presenting noise as
    /// evidence is worse than presenting none.
    #[test]
    fn noise_is_dropped() {
        for l in [
            "--------- beginning of main",
            "08-28 00:48:47.391    88    88 I hwcomposer: attach dmabuf: 2560x1440",
            "  10.5  1  1 I ActivityManager: Start proc 123",
            "",
        ] {
            assert!(parse_line(l, 0.0, 0.0).is_none(), "elenmeliydi: {l}");
        }
    }

    #[test]
    fn network_signatures_are_recognised() {
        for m in ["java.net.SocketTimeoutException: failed to connect",
                  "Unable to resolve host \"ads.example.com\"",
                  "java.net.UnknownHostException: x",
                  "Connection timed out"] {
            assert_eq!(classify("System.err", m), Some(Kind::Network), "{m}");
        }
    }

    /// Input-path events must be RECOGNISED: they are the only direct evidence
    /// for the user's question "is it the mouse system?".
    #[test]
    fn input_path_loss_is_recognised() {
        assert_eq!(classify("EventHub",
            "Removing device '/dev/input/wl_touch_events' due to inotify event"),
            Some(Kind::Input));
        assert_eq!(classify("EventHub",
            "Removed device: path=/dev/input/wl_touch_events name=wayland_touch"),
            Some(Kind::Input));
        // Unrelated EventHub noise must not count.
        assert_eq!(classify("EventHub", "usingClockIoctl=false"), None);
    }

    #[test]
    fn crashes_are_recognised() {
        assert_eq!(classify("tombstoned", "received crash request for pid 138"),
                   Some(Kind::Crash));
        assert_eq!(classify("libc", "Fatal signal 11 (SIGSEGV)"), Some(Kind::Crash));
    }

    #[test]
    fn anr_outranks_everything() {
        assert!(Kind::Anr.weight() > Kind::Network.weight());
        assert!(Kind::Network.weight() > Kind::Gc.weight());
    }

    #[test]
    fn hitches_use_the_threshold() {
        let iv = [(0.0, 5.0), (100.0, 40.0), (200.0, 6.0), (300.0, 120.0)];
        let h = hitches(&iv, 30.0);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].len_ms, 40.0);
        assert_eq!(h[1].t_ms, 300.0);
    }

    /// The evidence window must also cover BEFORE the stutter: the cause is
    /// usually logged just before it.
    #[test]
    fn correlation_looks_before_and_after() {
        let mut h = vec![Hitch { t_ms: 1000.0, len_ms: 100.0, evidence: vec![] }];
        let ev = vec![
            LogEvent { t_ms: 940.0, pid: 1, tag: "art".into(), kind: Kind::Gc,
                       msg: "GC freed".into() },
            LogEvent { t_ms: 1150.0, pid: 1, tag: "x".into(), kind: Kind::Network,
                       msg: "timeout".into() },
            LogEvent { t_ms: 5000.0, pid: 1, tag: "y".into(), kind: Kind::Lock,
                       msg: "uzak".into() },
        ];
        correlate(&mut h, &ev, 100.0, 100.0);
        assert_eq!(h[0].evidence.len(), 2, "events outside the window must not enter");
        // The highest-weight one first.
        assert_eq!(h[0].evidence[0].kind, Kind::Network);
    }

    /// 50 GC lines in one stutter must not make GC the prime suspect.
    #[test]
    fn tally_counts_hitches_not_lines() {
        let gc = |t: f64| LogEvent { t_ms: t, pid: 1, tag: "art".into(),
            kind: Kind::Gc, msg: "GC freed".into() };
        let net = |t: f64| LogEvent { t_ms: t, pid: 1, tag: "x".into(),
            kind: Kind::Network, msg: "timeout".into() };
        let hs = vec![
            Hitch { t_ms: 0.0, len_ms: 50.0,
                    evidence: (0..4).map(|i| gc(i as f64)).collect() },
            Hitch { t_ms: 100.0, len_ms: 50.0, evidence: vec![net(100.0)] },
            Hitch { t_ms: 200.0, len_ms: 50.0, evidence: vec![net(200.0)] },
        ];
        let v = verdicts(&hs, 1000, 1.0);
        assert_eq!(v[0].kind, Some(Kind::Network),
                   "network in 2 stutters, GC in 1 — network must lead: {v:?}");
    }

    /// With no evidence it must SAY so; it must not invent an accusation.
    #[test]
    fn unexplained_hitches_are_reported_as_such() {
        let hs = vec![Hitch { t_ms: 0.0, len_ms: 300.0, evidence: vec![] }];
        let v = verdicts(&hs, 100, 1.0);
        assert!(v.iter().any(|x| x.headline.contains("UNEXPLAINED")), "{v:?}");
        assert!(v.iter().all(|x| !x.headline.contains("garbage collection")));
    }

    /// Tag rates must be measured: it is the only way to find the noise
    /// drowning the diagnostic window.
    #[test]
    fn tag_rates_find_the_flooder() {
        let mut raw = String::new();
        for i in 0..300 {
            raw.push_str(&format!(
                "08-28 00:56:33.{i:03}    88    88 I hwcomposer: attach dmabuf\n"));
        }
        raw.push_str("08-28 00:56:34.000  100  100 I art: GC freed 1\n");
        let r = tag_rates(&raw, 2.0);
        assert_eq!(r[0].0, "hwcomposer");
        assert!((r[0].1 - 150.0).abs() < 0.1, "{r:?}");
        assert_eq!(r[1].0, "art");
    }

    #[test]
    fn tag_of_handles_both_formats() {
        assert_eq!(tag_of("08-28 00:56:33.146 88 88 I hwcomposer: x").as_deref(),
                   Some("hwcomposer"));
        assert_eq!(tag_of("  123.456 88 88 I Choreographer: x").as_deref(),
                   Some("Choreographer"));
        assert_eq!(tag_of("--------- beginning of main"), None);
    }

    /// If the host is not saturated it must NOT ACCUSE.
    #[test]
    fn host_verdict_stays_silent_without_evidence() {
        assert!(host_verdict(&HostFacts { unexplained: 4, ..Default::default() })
                .is_none());
        assert!(host_verdict(&HostFacts::default()).is_none());
    }

    /// If most are saturated it must say so — stopping at "unexplained" leaves
    /// the user with nothing.
    #[test]
    fn host_verdict_names_the_saturated_resource() {
        let v = host_verdict(&HostFacts {
            unexplained: 4, gpu_saturated: 3, ..Default::default() }).unwrap();
        assert!(v.headline.contains("GPU"), "{v:?}");
        let v = host_verdict(&HostFacts {
            unexplained: 4, vram_saturated: 4, ..Default::default() }).unwrap();
        assert!(v.headline.contains("VRAM"), "{v:?}");
    }

    #[test]
    fn no_hitches_says_so_plainly() {
        let v = verdicts(&[], 5000, 0.1);
        assert_eq!(v.len(), 1);
        assert!(v[0].headline.contains("No stutter"));
    }
}
