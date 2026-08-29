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
