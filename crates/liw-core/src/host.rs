//! Host state that breaks Waydroid without ever mentioning Waydroid.
//!
//! The case this module exists for: a kernel update deletes the RUNNING
//! kernel's module tree. Modules already loaded keep working, so the machine
//! looks healthy — but nothing new can ever be loaded again until reboot.
//!
//! Waydroid then fails to start with a firewall error. On this machine the
//! error was:
//!
//! ```text
//! Error: No such file or directory; did you mean table 'unwall' in family ip?
//! ```
//!
//! Every part of that message points somewhere else. It names a foreign
//! firewall table, so it reads as a firewall conflict; it is reported against
//! the wrong line, because nft's location tracking for command-line rulesets
//! is unreliable; and "No such file or directory" is a bare ENOENT that says
//! nothing about modules. Bisecting the ruleset one statement at a time showed
//! the only failing statement was `masquerade`, which needs `nft_masq` — a
//! module that was not loaded and could no longer be loaded.
//!
//! Diagnosing this from the symptom is close to impossible. Diagnosing it from
//! the host is two lines, which is why it belongs here.

use serde::{Deserialize, Serialize};

/// The running kernel's modules are gone from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleModules {
    /// The kernel that is running, from `uname -r`.
    pub running: String,
    /// Module trees that DO exist — normally the version just installed.
    pub available: Vec<String>,
}

impl StaleModules {
    /// One line for a human.
    pub fn summary(&self) -> String {
        format!(
            "the running kernel is {} but its modules are gone; \
             only {} present on disk. A kernel update removed them. \
             Modules already loaded still work, but no new module can be \
             loaded until reboot.",
            self.running,
            self.available.join(", "))
    }
}

/// Detects a kernel whose module tree has been deleted underneath it.
///
/// Returns `None` when nothing can be concluded. An empty or unreadable
/// module directory is NOT evidence of a problem — it is evidence of not
/// knowing, and reporting it as a fault would send the reader after a
/// non-existent bug.
pub fn stale_module_tree(running: &str, present: &[String]) -> Option<StaleModules> {
    let running = running.trim();
    if running.is_empty() || present.is_empty() {
        return None;
    }
    if present.iter().any(|p| p.trim() == running) {
        return None;
    }
    Some(StaleModules {
        running: running.to_string(),
        available: present.iter().map(|s| s.trim().to_string()).collect(),
    })
}

/// Reads the running kernel release.
pub fn running_kernel() -> Option<String> {
    let out = std::process::Command::new("uname").arg("-r").output().ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Lists the module trees present on disk.
pub fn module_trees() -> Vec<String> {
    for dir in ["/usr/lib/modules", "/lib/modules"] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut v: Vec<String> = rd.flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        if !v.is_empty() {
            v.sort();
            return v;
        }
    }
    Vec::new()
}

/// The whole check, done for real.
pub fn check_modules() -> Option<StaleModules> {
    stale_module_tree(&running_kernel()?, &module_trees())
}

// ---------------------------------------------------------------------------
// Individual modules
// ---------------------------------------------------------------------------

/// Whether a specific kernel module can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleState {
    /// Loaded, or built into the kernel. Either way it is usable.
    Present,
    /// Not loaded, but a file exists for the running kernel — it can load.
    Loadable,
    /// Not loaded and no file for the running kernel. It can never load.
    Missing,
    /// Could not be determined; the module index was unreadable.
    Unknown,
}

/// A module Waydroid needs, and where Waydroid itself asks for it.
///
/// The `used_by` text is not commentary — each one is a command observed in
/// Waydroid's own scripts or log. Nothing here is inferred from a symptom.
#[derive(Debug, Clone, Copy)]
pub struct Requirement {
    pub module: &'static str,
    pub used_by: &'static str,
}

pub const WAYDROID_MODULES: &[Requirement] = &[
    Requirement { module: "nft_masq",
        used_by: "the `masquerade` rule in waydroid-net.sh" },
    Requirement { module: "ashmem_linux",
        used_by: "`modprobe -q ashmem_linux` at container start" },
    Requirement { module: "binder_linux",
        used_by: "`mount -t binder binder /dev/binderfs`" },
];

/// Names of currently loaded modules, from /proc/modules.
fn loaded_modules() -> Vec<String> {
    std::fs::read_to_string("/proc/modules")
        .map(|s| s.lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect())
        .unwrap_or_default()
}

/// Module names the running kernel has FILES for, from modules.dep.
///
/// Reading the index once beats shelling out to `modinfo` per module: it needs
/// no external binary and gives the same answer.
fn indexed_modules(release: &str) -> Option<Vec<String>> {
    for base in ["/usr/lib/modules", "/lib/modules"] {
        let dep = format!("{base}/{release}/modules.dep");
        let Ok(s) = std::fs::read_to_string(&dep) else { continue };
        return Some(s.lines()
            .filter_map(|l| l.split(':').next())
            .filter_map(module_name_of_path)
            .collect());
    }
    None
}

/// `kernel/net/netfilter/nft_masq.ko.zst` -> `nft_masq`
///
/// Compression suffixes vary by distribution (.zst, .xz, .gz, none), so the
/// extension is stripped down to the `.ko` rather than matched against a list.
pub fn module_name_of_path(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let ko = file.find(".ko")?;
    let name = &file[..ko];
    (!name.is_empty()).then(|| name.replace('-', "_"))
}

/// Measures one module's state. Nothing here is assumed.
pub fn module_state(name: &str) -> ModuleState {
    let want = name.replace('-', "_");
    if loaded_modules().iter().any(|m| *m == want) {
        return ModuleState::Present;
    }
    // Built-in modules never appear in /proc/modules but do get a sysfs entry.
    // Without this check a kernel with binder compiled in reads as broken.
    if std::path::Path::new(&format!("/sys/module/{want}")).exists() {
        return ModuleState::Present;
    }
    let Some(release) = running_kernel() else { return ModuleState::Unknown };
    match indexed_modules(&release) {
        None => ModuleState::Unknown,
        Some(idx) if idx.iter().any(|m| *m == want) => ModuleState::Loadable,
        Some(_) => ModuleState::Missing,
    }
}

/// What Waydroid needs, and what was actually found for each.
pub fn waydroid_module_report() -> Vec<(Requirement, ModuleState)> {
    WAYDROID_MODULES.iter().map(|r| (*r, module_state(r.module))).collect()
}

/// The modules Waydroid needs that CANNOT be loaded.
pub fn unloadable(report: &[(Requirement, ModuleState)]) -> Vec<Requirement> {
    report.iter()
        .filter(|(_, s)| *s == ModuleState::Missing)
        .map(|(r, _)| *r)
        .collect()
}

// ---------------------------------------------------------------------------
// Watching for the change while we run
// ---------------------------------------------------------------------------

/// Latches the kernel-module state so a change is reported ONCE.
///
/// A kernel update lands while the daemon is already running, and the machine
/// gives no other sign. Without a watch the first symptom is a session that
/// will not start, hours later, with an error naming a firewall.
///
/// The latch matters as much as the check: a warning repeated every poll is
/// noise, and noise is how a real warning gets missed.
#[derive(Debug, Default)]
pub struct KernelWatch {
    stale: Option<bool>,
}

impl KernelWatch {
    pub fn new() -> Self { Self { stale: None } }

    /// Feeds in the current state. Returns `Some(is_stale)` only when it
    /// CHANGED — including the very first observation, which is worth logging.
    pub fn poll(&mut self, now_stale: bool) -> Option<bool> {
        if self.stale == Some(now_stale) { return None; }
        self.stale = Some(now_stale);
        Some(now_stale)
    }

    /// Last known state, if it has been polled at all.
    pub fn is_stale(&self) -> Option<bool> { self.stale }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// The real case: kernel 7.2.0 running, only 7.2.2 and the LTS on disk.
    #[test]
    fn updated_kernel_leaves_the_running_one_without_modules() {
        let s = stale_module_tree(
            "7.2.0-1-cachyos",
            &v(&["6.18.42-1-cachyos-lts", "7.2.2-1-cachyos"])).unwrap();
        assert_eq!(s.running, "7.2.0-1-cachyos");
        assert_eq!(s.available.len(), 2);
    }

    #[test]
    fn matching_tree_is_not_a_fault() {
        assert!(stale_module_tree("7.2.2-1-cachyos",
            &v(&["6.18.42-1-cachyos-lts", "7.2.2-1-cachyos"])).is_none());
    }

    /// Not being able to look is not the same as finding a problem. Reporting
    /// a fault here would send the reader chasing something that is not there.
    #[test]
    fn unreadable_module_dir_claims_nothing() {
        assert!(stale_module_tree("7.2.0-1-cachyos", &[]).is_none());
    }

    #[test]
    fn unknown_kernel_claims_nothing() {
        assert!(stale_module_tree("", &v(&["7.2.2-1-cachyos"])).is_none());
    }

    /// A trailing newline from `uname -r` must not make every kernel look stale.
    #[test]
    fn whitespace_does_not_create_a_false_positive() {
        assert!(stale_module_tree("7.2.2-1-cachyos\n",
            &v(&["7.2.2-1-cachyos"])).is_none());
    }

    #[test]
    fn summary_names_the_running_kernel_and_what_is_there() {
        let s = stale_module_tree("7.2.0", &v(&["7.2.2"])).unwrap();
        let t = s.summary();
        assert!(t.contains("7.2.0"), "{t}");
        assert!(t.contains("7.2.2"), "{t}");
        assert!(t.contains("reboot"), "must say what fixes it: {t}");
    }

    // --- module measurement -------------------------------------------

    #[test]
    fn module_name_is_read_off_any_compression_suffix() {
        for (path, want) in [
            ("kernel/net/netfilter/nft_masq.ko.zst", "nft_masq"),
            ("kernel/net/netfilter/nft_masq.ko.xz",  "nft_masq"),
            ("kernel/net/netfilter/nft_masq.ko",     "nft_masq"),
            ("kernel/drivers/android/binder_linux.ko.gz", "binder_linux"),
        ] {
            assert_eq!(module_name_of_path(path).as_deref(), Some(want), "{path}");
        }
    }

    /// Distributions write some module files with hyphens while the kernel
    /// reports underscores. Both must resolve to the same name.
    #[test]
    fn hyphens_and_underscores_are_the_same_module() {
        assert_eq!(module_name_of_path("kernel/x/foo-bar.ko.zst").as_deref(),
                   Some("foo_bar"));
    }

    #[test]
    fn a_path_with_no_module_in_it_is_not_a_module() {
        assert_eq!(module_name_of_path("kernel/net/netfilter/"), None);
        assert_eq!(module_name_of_path(""), None);
    }

    /// Every requirement must say where Waydroid asks for it. A bare module
    /// name in a report is exactly the kind of unsourced claim to avoid.
    #[test]
    fn every_requirement_cites_its_source() {
        assert!(!WAYDROID_MODULES.is_empty());
        for r in WAYDROID_MODULES {
            assert!(!r.module.is_empty());
            assert!(r.used_by.len() > 10, "{} has no source", r.module);
        }
    }

    /// A module that is loaded right now must never read as missing. Reading
    /// /proc/modules is the whole point; asserting instead would be a guess.
    #[test]
    fn a_loaded_module_is_present() {
        let loaded = loaded_modules();
        if let Some(m) = loaded.first() {
            assert_eq!(module_state(m), ModuleState::Present, "{m} is loaded");
        }
    }

    #[test]
    fn a_module_that_does_not_exist_is_not_present() {
        assert_ne!(module_state("liwinux_no_such_module"), ModuleState::Present);
    }

    /// Only MISSING is actionable. Loadable and Unknown must never be counted
    /// as faults, or a healthy machine gets told to reboot.
    #[test]
    fn only_missing_modules_are_reported_as_faults() {
        let r = |s| (Requirement { module: "x", used_by: "somewhere in waydroid" }, s);
        let rep = vec![r(ModuleState::Present), r(ModuleState::Loadable),
                       r(ModuleState::Unknown)];
        assert!(unloadable(&rep).is_empty());
        let rep2 = vec![r(ModuleState::Missing)];
        assert_eq!(unloadable(&rep2).len(), 1);
    }

    // --- the watch ------------------------------------------------------

    /// The first observation is always worth reporting; after that only
    /// changes are, or the log fills with the same line forever.
    #[test]
    fn first_observation_is_reported_then_silence() {
        let mut w = KernelWatch::new();
        assert_eq!(w.poll(false), Some(false), "first look must be reported");
        assert_eq!(w.poll(false), None, "unchanged state must stay quiet");
        assert_eq!(w.poll(false), None);
    }

    #[test]
    fn going_stale_is_reported_once() {
        let mut w = KernelWatch::new();
        w.poll(false);
        assert_eq!(w.poll(true), Some(true), "the change must surface");
        assert_eq!(w.poll(true), None, "and only once");
    }

    /// Recovering (a reboot) is a change too, and worth saying.
    #[test]
    fn recovery_is_reported() {
        let mut w = KernelWatch::new();
        w.poll(true);
        assert_eq!(w.poll(false), Some(false));
    }

    #[test]
    fn state_is_unknown_before_the_first_poll() {
        assert_eq!(KernelWatch::new().is_stale(), None);
    }

    /// The live check must not panic or invent a fault on a healthy machine.
    #[test]
    fn live_check_is_consistent_with_the_pure_one() {
        let running = running_kernel();
        let trees = module_trees();
        assert_eq!(
            check_modules(),
            running.and_then(|r| stale_module_tree(&r, &trees)));
    }
}
