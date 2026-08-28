//! `liw perf` — diagnosis of performance levers.
//!
//! This command CHANGES NOTHING. It reads the state of the system and reports
//! where performance is being left behind. Applying is a separate step.

use anyhow::Result;
use liw_core::perf::{self, Finding, Impact, Status, VirglClient};
use std::process::Command;

/// Reads a file; returns an empty string if absent (the lever counts as "missing").
fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Runs a command and returns its stdout. Empty string on failure.
///
/// The exit code IS CHECKED: treating a failed command's stdout as valid data
/// previously caused a silent misdiagnosis.
fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Age of a process in seconds. 0 if absent.
fn process_age(pattern: &str) -> u64 {
    run("pgrep", &["-f", pattern]).lines().next()
        .map(|pid| run("ps", &["-o", "etimes=", "-p", pid.trim()]))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Finds the roots of the virgl client trees.
///
/// Tree shape: the main `virgl_test_server` forks a child per client, and that
/// child spawns the render servers. The number of clients equals the MAIN
/// PROCESS'S DIRECT CHILDREN — not the total process count.
fn virgl_clients() -> Vec<VirglClient> {
    let raw = run("ps", &["-eo", "pid=,ppid=,etimes=,args="]);
    let rows: Vec<(u32, u32, u64)> = raw.lines()
        .filter(|l| l.contains("virgl_test_server"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?,
                  it.next()?.parse().ok()?))
        })
        .collect();

    // Main process: spawned by an ancestor that is not virgl_test_server.
    let pids: Vec<u32> = rows.iter().map(|(p, _, _)| *p).collect();
    let masters: Vec<u32> = rows.iter()
        .filter(|(_, pp, _)| !pids.contains(pp))
        .map(|(p, _, _)| *p).collect();

    rows.iter()
        .filter(|(_, pp, _)| masters.contains(pp))
        .map(|(pid, _, age_s)| VirglClient { pid: *pid, age_s: *age_s })
        .collect()
}

pub fn status() -> Result<()> {
    const CPU: &str = "/sys/devices/system/cpu/cpu0/cpufreq";

    let refresh = perf::parse_active_refresh(&run("kscreen-doctor", &["-o"]));
    // The session's graphics age comes from SurfaceFlinger: this session's GPU
    // clients must have been born after it.
    let session_age = process_age("surfaceflinger");

    let findings = vec![
        perf::governor(
            &read(&format!("{CPU}/scaling_governor")),
            &read(&format!("{CPU}/scaling_available_governors")),
            &read(&format!("{CPU}/scaling_driver"))),
        perf::epp(&read(&format!("{CPU}/energy_performance_preference"))),
        perf::powermizer(&run("nvidia-settings",
            &["-q", "[gpu:0]/GPUPowerMizerMode", "-t"])),
        perf::refresh_budget(refresh.unwrap_or(0.0)),
        perf::orphan_clients(&virgl_clients(), session_age),
        perf::container_weight(&run("systemctl",
            &["show", "waydroid-container", "-p", "CPUWeight"])),
    ];

    report(&findings);
    Ok(())
}

fn report(findings: &[Finding]) {
    println!("\n  Performance diagnosis\n  {}", "─".repeat(60));
    for f in findings {
        let (mark, label) = match f.status {
            Status::Optimal => ("✓", "hedefte"),
            Status::Improvable => ("!", "improvable"),
            Status::Unavailable => ("·", "not available here"),
        };
        println!("\n  {mark} {}  [{label}]", f.title);
        println!("      current: {}", f.current);
        if f.status == Status::Improvable {
            println!("      hedef : {}", f.target);
        }
        if f.impact == Impact::Unknown && f.status == Status::Improvable {
            println!("      impact : NOT MEASURED");
        }
        for line in wrap(&f.note, 66) {
            println!("      {line}");
        }
    }

    let (ok, imp, na) = perf::summarise(findings);
    println!("\n  {}", "─".repeat(60));
    println!("  {ok} on target · {imp} improvable · {na} unavailable\n");
    if imp > 0 {
        println!("  None of these has been MEASURED on this system. Take a");
        println!("  baseline before applying anything:\n");
        println!("      liw bench <paket> --duration 60\n");
    }
}

/// Wraps a note at word boundaries.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for w in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() { line.push(' '); }
        line.push_str(w);
    }
    if !line.is_empty() { out.push(line); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_respects_width_and_keeps_all_words() {
        let s = "one two three four five six seven eight nine ten";
        let lines = wrap(s, 12);
        assert!(lines.iter().all(|l| l.chars().count() <= 12), "{lines:?}");
        assert_eq!(lines.join(" "), s, "no word may be lost");
    }

    #[test]
    fn wrap_handles_empty() {
        assert!(wrap("", 20).is_empty());
    }

    /// A single word longer than the width must still not be lost.
    #[test]
    fn wrap_keeps_overlong_word() {
        assert_eq!(wrap("anextremelylongword", 5), vec!["anextremelylongword"]);
    }

    #[test]
    fn missing_file_reads_as_empty_not_panic() {
        assert_eq!(read("/proc/kesinlikle/olmayan/dosya"), "");
    }
}
