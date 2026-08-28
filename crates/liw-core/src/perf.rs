//! Diagnosis of performance levers.
//!
//! Principle: MEASURE FIRST, APPLY LATER. This module changes nothing; it only
//! reads the current state of the system and reports where performance is being
//! left behind. Applying is a separate step needing separate consent.
//!
//! Every function here is PURE: it takes raw text and returns a finding. That
//! makes it testable without root and without real hardware.

use serde::{Deserialize, Serialize};

/// The measured impact of a lever.
///
/// These are not GUESSES: until measured the value is `Unknown`. Calling a
/// lever "high impact" without measuring is exactly what we avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Impact {
    /// Measured, and it made a clear difference in frame time.
    Measured,
    /// Not measured yet. The default.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Zaten hedefte.
    Optimal,
    /// Not on target; can be improved.
    Improvable,
    /// This lever does not exist on this machine.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: &'static str,
    pub title: &'static str,
    pub current: String,
    pub target: String,
    pub status: Status,
    pub impact: Impact,
    /// Why it matters — or why it might not.
    pub note: String,
}

impl Finding {
    fn unavailable(id: &'static str, title: &'static str, why: &str) -> Self {
        Self { id, title, current: "yok".into(), target: "-".into(),
            status: Status::Unavailable, impact: Impact::Unknown, note: why.into() }
    }
}

/// CPU frequency governor.
///
/// Worth stating that the name `powersave` is misleading on the `intel_pstate`
/// driver: that mode still reaches the maximum frequency and its difference
/// from `performance` is usually small. So we promise no big win without
/// measuring.
pub fn governor(current: &str, available: &str, driver: &str) -> Finding {
    let cur = current.trim();
    if cur.is_empty() {
        return Finding::unavailable("cpu.governor", "CPU governor",
            "no cpufreq interface found");
    }
    let has_perf = available.split_whitespace().any(|g| g == "performance");
    let pstate = driver.trim() == "intel_pstate";
    let note = if pstate {
        "on intel_pstate, 'powersave' still reaches the top frequency; the \
         difference is usually small. No gain is promised without measuring."
    } else {
        "demand-based governors raise the frequency late, which can produce \
         gecikmesi yaratabilir."
    };
    Finding {
        id: "cpu.governor", title: "CPU governor",
        current: cur.to_string(),
        target: if has_perf { "performance".into() } else { "-".into() },
        status: if cur == "performance" { Status::Optimal }
                else if has_perf { Status::Improvable }
                else { Status::Unavailable },
        impact: Impact::Unknown, note: note.into(),
    }
}

/// Energy/performance preference (intel_pstate + HWP only).
pub fn epp(current: &str) -> Finding {
    let cur = current.trim();
    if cur.is_empty() {
        return Finding::unavailable("cpu.epp", "Enerji/performans tercihi",
            "HWP EPP is not enabled on this CPU");
    }
    Finding {
        id: "cpu.epp", title: "Enerji/performans tercihi",
        current: cur.to_string(), target: "performance".into(),
        status: if cur == "performance" { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "EPP acts more directly than the governor: it decides how \
               aggressively turbo is held.".into(),
    }
}

/// NVIDIA PowerMizer mode. 0 = adaptive, 1 = prefer maximum performance.
pub fn powermizer(raw: &str) -> Finding {
    let cur = raw.trim();
    let Ok(mode) = cur.parse::<i32>() else {
        return Finding::unavailable("gpu.powermizer", "NVIDIA PowerMizer",
            "could not query nvidia-settings (may need an X server under Wayland)");
    };
    Finding {
        id: "gpu.powermizer", title: "NVIDIA PowerMizer",
        current: format!("{mode} ({})", match mode {
            0 => "adaptive", 1 => "max performance",
            2 => "otomatik", _ => "bilinmiyor" }),
        target: "1 (azami performans)".into(),
        status: if mode == 1 { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "In adaptive mode the clock rises and falls with load; the \
               transitions can produce the occasional long frame.".into(),
    }
}

/// Display refresh rate — this is what sets the game's target FPS.
///
/// A high refresh rate is NOT bad. It is shown as a finding because it sets the
/// frame budget: 5.6 ms at 180 Hz, 16.7 ms at 60 Hz. If frames are being missed,
/// lowering it buys consistency; if they are not, leave it alone.
/// there is no need to touch it.
pub fn refresh_budget(hz: f64) -> Finding {
    let budget = if hz > 0.0 { 1000.0 / hz } else { 0.0 };
    Finding {
        id: "display.refresh", title: "Display refresh / frame budget",
        current: format!("{hz:.0} Hz — {budget:.1} ms per frame"),
        target: "depends on measurement".into(),
        status: Status::Optimal,
        impact: Impact::Unknown,
        note: "A high refresh rate is not a problem in itself. Only if the \
               miss rate is high does lowering it buy consistency.".into(),
    }
}

/// Finds the active output's refresh rate from `kscreen-doctor -o` output.
///
/// The active mode is marked with `*`. With several displays the HIGHEST
/// refresh active mode is taken: that is where the game will be running.
pub fn parse_active_refresh(raw: &str) -> Option<f64> {
    let mut best: Option<f64> = None;
    for tok in raw.split_whitespace() {
        // Strip colour escapes; look for "2560x1440@180.00*".
        let t: String = strip_ansi(tok);
        if !t.contains('*') { continue; }
        let Some((_, hz)) = t.split_once('@') else { continue };
        let hz: String = hz.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if let Ok(v) = hz.parse::<f64>() {
            if best.is_none_or(|b| v > b) { best = Some(v); }
        }
    }
    best
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if c == '\u{1b}' { in_esc = true; continue; }
        if in_esc { if c.is_ascii_alphabetic() { in_esc = false; } continue; }
        out.push(c);
    }
    out
}

/// A virgl GPU client (the root of one tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirglClient {
    pub pid: u32,
    pub age_s: u64,
}

/// Finds virgl clients left over from a previous session.
///
/// CAREFUL — this was misdiagnosed once: the process COUNT was taken as the
/// criterion. Wrong. Every Android process opens its own GPU context, so DOZENS
/// of clients in a single session is NORMAL.
///
/// The correct criterion is an age comparison: a client is left over only if it
/// is OLDER than SurfaceFlinger, because this session's clients must have been
/// born after it.
///
/// If `session_age_s` is zero the session is not running, and every client
/// still standing is an orphan.
pub fn orphan_clients(clients: &[VirglClient], session_age_s: u64) -> Finding {
    /// Tolerance for measurement noise around birth order.
    const MARGIN_S: u64 = 5;

    let orphans = clients.iter()
        .filter(|c| c.age_s > session_age_s.saturating_add(MARGIN_S))
        .count();
    let live = clients.len() - orphans;

    Finding {
        id: "render.orphans", title: "Orphaned virgl clients",
        current: if session_age_s == 0 {
            format!("{orphans} orphaned (session not running)")
        } else {
            format!("{orphans} orphaned, {live} live ({} clients)", clients.len())
        },
        target: "0".into(),
        status: if orphans > 0 { Status::Improvable } else { Status::Optimal },
        impact: Impact::Unknown,
        note: "Dozens of live clients is normal: every Android process opens \
               its own GPU context. Only those older than SurfaceFlinger are \
               left over from a previous session.".into(),
    }
}

/// CPU weight of the systemd unit.
pub fn container_weight(raw: &str) -> Finding {
    let set = raw.lines()
        .find_map(|l| l.strip_prefix("CPUWeight="))
        .map(|v| v.trim())
        .filter(|v| *v != "[not set]" && !v.is_empty());
    Finding {
        id: "container.cpuweight", title: "Container CPU weight",
        current: set.unwrap_or("unset (default 100)").to_string(),
        target: "high (e.g. 300)".into(),
        status: if set.is_some() { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "Only has an effect while the CPU is SATURATED. On an idle \
               system the difference is unmeasurable — hence no promise.".into(),
    }
}

/// Renders the finding list as a human-readable table.
pub fn summarise(findings: &[Finding]) -> (usize, usize, usize) {
    let mut ok = 0; let mut imp = 0; let mut na = 0;
    for f in findings {
        match f.status {
            Status::Optimal => ok += 1,
            Status::Improvable => imp += 1,
            Status::Unavailable => na += 1,
        }
    }
    (ok, imp, na)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_at_performance_is_optimal() {
        let f = governor("performance\n", "performance powersave", "intel_pstate");
        assert_eq!(f.status, Status::Optimal);
    }

    #[test]
    fn governor_powersave_is_improvable() {
        let f = governor("powersave\n", "performance powersave", "intel_pstate");
        assert_eq!(f.status, Status::Improvable);
        assert_eq!(f.target, "performance");
    }

    /// We must not overpromise on intel_pstate; the note must say so.
    #[test]
    fn pstate_note_is_honest() {
        let f = governor("powersave", "performance powersave", "intel_pstate");
        assert!(f.note.contains("difference"), "the note must be realistic: {}", f.note);
    }

    #[test]
    fn missing_cpufreq_is_unavailable() {
        assert_eq!(governor("", "", "").status, Status::Unavailable);
        assert_eq!(epp("").status, Status::Unavailable);
    }

    #[test]
    fn powermizer_modes() {
        assert_eq!(powermizer("1").status, Status::Optimal);
        assert_eq!(powermizer("0").status, Status::Improvable);
        assert_eq!(powermizer("hata").status, Status::Unavailable);
    }

    /// Real kscreen-doctor output arrives with colour escapes.
    #[test]
    fn parses_refresh_from_real_kscreen_output() {
        let raw = "\u{1b}[01;34mModes: \u{1b}[0;0m 25:2560x1440@60.00!  \
                   26:\u{1b}[01;32m2560x1440@180.00*\u{1b}[0;0m  27:2560x1440@165.00";
        assert_eq!(parse_active_refresh(raw), Some(180.0));
    }

    /// With two displays the high-refresh one, where the game runs, must win.
    #[test]
    fn picks_highest_active_refresh_across_outputs() {
        let raw = "1:1920x1080@60.00*  26:2560x1440@180.00*";
        assert_eq!(parse_active_refresh(raw), Some(180.0));
    }

    #[test]
    fn no_active_mode_yields_none() {
        assert_eq!(parse_active_refresh("1:1920x1080@60.00  2:800x600@75.00"), None);
    }

    #[test]
    fn refresh_budget_is_inverse_of_hz() {
        let f = refresh_budget(180.0);
        assert!(f.current.contains("5.6"), "wrong budget: {}", f.current);
        // A high refresh rate must not be flagged as a defect.
        assert_eq!(f.status, Status::Optimal);
    }

    fn cl(pid: u32, age_s: u64) -> VirglClient { VirglClient { pid, age_s } }

    /// Real measurement: 10 clients, all younger than SurfaceFlinger (40497s).
    /// These are LIVE — they were once wrongly taken for orphans.
    #[test]
    fn many_clients_younger_than_session_are_all_live() {
        let clients: Vec<_> = [40295, 40291, 40289, 40288, 40283, 40277,
                               40239, 39986, 40294, 377]
            .iter().enumerate().map(|(i, a)| cl(i as u32, *a)).collect();
        let f = orphan_clients(&clients, 40497);
        assert_eq!(f.status, Status::Optimal, "{}", f.current);
        assert!(f.current.contains("0 orphaned"), "{}", f.current);
    }

    /// A client OLDER than the session is a genuine orphan.
    #[test]
    fn clients_older_than_session_are_orphans() {
        let f = orphan_clients(&[cl(1, 90000), cl(2, 100), cl(3, 50)], 1000);
        assert_eq!(f.status, Status::Improvable);
        assert!(f.current.contains("1 orphaned"), "{}", f.current);
    }

    /// With no session, every client still standing is an orphan.
    #[test]
    fn without_session_everything_is_orphan() {
        let f = orphan_clients(&[cl(1, 100), cl(2, 50)], 0);
        assert_eq!(f.status, Status::Improvable);
        assert!(f.current.contains("2 orphaned"), "{}", f.current);
    }

    #[test]
    fn no_clients_is_optimal() {
        assert_eq!(orphan_clients(&[], 1000).status, Status::Optimal);
    }

    #[test]
    fn unset_cpuweight_is_improvable() {
        let f = container_weight("CPUWeight=[not set]\nIOWeight=[not set]\n");
        assert_eq!(f.status, Status::Improvable);
        let f = container_weight("CPUWeight=300\n");
        assert_eq!(f.status, Status::Optimal);
    }

    /// No unmeasured lever may be marked as "effective".
    #[test]
    fn nothing_claims_measured_impact_before_measurement() {
        let all = [governor("powersave", "performance", "intel_pstate"),
                   epp("balance_performance"), powermizer("0"),
                   refresh_budget(180.0),
                   orphan_clients(&[cl(1, 90000)], 1000),
                   container_weight("CPUWeight=[not set]")];
        for f in &all {
            assert_eq!(f.impact, Impact::Unknown,
                "{} claims impact without measurement", f.id);
        }
        let (ok, imp, _) = summarise(&all);
        assert_eq!((ok, imp), (1, 5));
    }
}
