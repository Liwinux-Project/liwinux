//! Profile store: discovers profiles, loads them and matches them to packages.
//!
//! # Precedence
//!
//! User profiles **shadow** system profiles. To edit a distributed profile the
//! user copies it; when an update arrives their change is not lost.
//!
//!
//! ```text
//! $XDG_CONFIG_HOME/liwinux/profiles/   (user — wins)
//! /usr/share/liwinux/profiles/         (sistem)
//! ./profiles/                          (development)
//! ```

use crate::profile::{Profile, ProfileError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a profile came from. Carried so we can answer "which file is
/// actually running".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// Highest precedence.
    User,
    System,
    /// From inside the repository; development only.
    Bundled,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub profile: Profile,
    pub path: PathBuf,
    pub origin: Origin,
}

/// A profile that failed to load. Not swallowed silently: a broken file must
/// be reported, otherwise "why is my profile not working" goes unanswered.
///
#[derive(Debug)]
pub struct BadProfile {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Store {
    /// package name -> highest-precedence entry
    by_package: BTreeMap<String, Entry>,
    pub problems: Vec<BadProfile>,
}

impl Store {
    /// Scans the default directories.
    pub fn discover() -> Self {
        Self::from_dirs(&default_dirs())
    }

    pub fn from_dirs(dirs: &[(PathBuf, Origin)]) -> Self {
        let mut s = Self::default();
        for (dir, origin) in dirs {
            s.scan_dir(dir, *origin);
        }
        s
    }

    fn scan_dir(&mut self, dir: &Path, origin: Origin) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "toml") { continue; }
            match std::fs::read_to_string(&path)
                .map_err(|e| ProfileError::Io(e))
                .and_then(|t| Profile::from_toml(&t))
            {
                Ok(profile) => {
                    let entry = Entry { profile, path: path.clone(), origin };
                    let pkg = entry.profile.package.clone();
                    match self.by_package.get(&pkg) {
                        // Higher precedence (smaller Origin) wins.
                        Some(prev) if prev.origin < entry.origin => {}
                        // Two profiles at the SAME precedence claim one package.
                        // Picking one silently produces non-deterministic
                        // behaviour: which one loads depends on directory read
                        // order and the user says "sometimes my old profile runs".
                        Some(prev) if prev.origin == entry.origin => {
                            let (keep, drop) = if prev.path <= entry.path {
                                (prev.path.clone(), path.clone())
                            } else {
                                let old = prev.path.clone();
                                self.by_package.insert(pkg.clone(), entry);
                                (path.clone(), old)
                            };
                            self.problems.push(BadProfile {
                                path: drop.clone(),
                                error: format!(
                                    "'{pkg}' paketini iki profil birden iddia ediyor; \
                                     using '{}'. Delete one or rename the package.",
                                    keep.display()),
                            });
                        }
                        _ => { self.by_package.insert(pkg, entry); }
                    }
                }
                Err(err) => self.problems.push(BadProfile {
                    path, error: err.to_string(),
                }),
            }
        }
    }

    pub fn for_package(&self, pkg: &str) -> Option<&Entry> {
        self.by_package.get(pkg)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.by_package.values()
    }

    pub fn len(&self) -> usize { self.by_package.len() }
    pub fn is_empty(&self) -> bool { self.by_package.is_empty() }
}

/// Directories to search, in precedence order.
///
/// THE WORKING DIRECTORY IS NOT CONSULTED. If it were, the same command would
/// behave differently from different directories and the user would say "my
/// profile is sometimes found" — a hard-to-diagnose, embarrassing class of bug.
pub fn default_dirs() -> Vec<(PathBuf, Origin)> {
    let mut dirs: Vec<(PathBuf, Origin)> = Vec::new();
    if let Some(cfg) = user_profile_dir() {
        dirs.push((cfg, Origin::User));
    }
    dirs.push((PathBuf::from("/usr/share/liwinux/profiles"), Origin::System));
    // Development convenience: the repository next to the executable.
    // Based on the binary's location, NOT the working directory.
    if let Some(d) = bundled_dir() {
        dirs.push((d, Origin::Bundled));
    }
    if let Some(env) = std::env::var_os("LIWINUX_PROFILE_DIR") {
        dirs.push((PathBuf::from(env), Origin::User));
    }
    dirs
}

/// `target/{debug,release}/liw` -> `profiles/` at the repository root.
fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    // Look at most four levels up; more would wander into unrelated directories.
    for _ in 0..4 {
        let cand = dir.join("profiles");
        if cand.is_dir() { return Some(cand); }
        dir = dir.parent()?;
    }
    None
}

pub fn user_profile_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("liwinux").join("profiles"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, pkg: &str, binding_key: u16) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), format!(r#"
name = "{name}"
package = "{pkg}"
[bindings.zipla]
type = "tap"
trigger = {{ Key = {binding_key} }}
at = {{ x = 0.5, y = 0.5 }}
"#)).unwrap();
    }

    fn tmp(sub: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("liw-store-{}-{}", std::process::id(), sub));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn finds_profile_by_package() {
        let d = tmp("a");
        write(&d, "x.toml", "com.example.game", 17);
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert!(s.for_package("com.example.game").is_some());
        assert!(s.for_package("com.absent").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A user profile must shadow a system profile.
    #[test]
    fn user_profile_shadows_system() {
        let sys = tmp("sys");
        let usr = tmp("usr");
        write(&sys, "g.toml", "com.example.game", 17);
        write(&usr, "g.toml", "com.example.game", 30);
        let s = Store::from_dirs(&[
            (usr.clone(), Origin::User),
            (sys.clone(), Origin::System),
        ]);
        let e = s.for_package("com.example.game").unwrap();
        assert_eq!(e.origin, Origin::User);
        let _ = std::fs::remove_dir_all(&sys);
        let _ = std::fs::remove_dir_all(&usr);
    }

    /// Scan order must not affect precedence.
    #[test]
    fn precedence_independent_of_scan_order() {
        let sys = tmp("sys2");
        let usr = tmp("usr2");
        write(&sys, "g.toml", "com.example.game", 17);
        write(&usr, "g.toml", "com.example.game", 30);
        let s = Store::from_dirs(&[
            (sys.clone(), Origin::System),
            (usr.clone(), Origin::User),
        ]);
        assert_eq!(s.for_package("com.example.game").unwrap().origin, Origin::User);
        let _ = std::fs::remove_dir_all(&sys);
        let _ = std::fs::remove_dir_all(&usr);
    }

    /// A broken profile must not hide the others, nor be swallowed silently.
    #[test]
    fn broken_profile_is_reported_and_others_still_load() {
        let d = tmp("bad");
        write(&d, "good.toml", "com.example.good", 17);
        std::fs::write(d.join("broken.toml"), "this is not valid toml [[[").unwrap();
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert!(s.for_package("com.example.good").is_some(), "the good profile must load");
        assert_eq!(s.problems.len(), 1, "the broken profile must be reported");
        assert!(s.problems[0].path.ends_with("broken.toml"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A profile failing validation must also be reported as a problem.
    #[test]
    fn invalid_profile_is_a_reported_problem() {
        let d = tmp("dup");
        std::fs::write(d.join("dup.toml"), r#"
name = "d"
package = "com.example.d"
[bindings.a]
type = "tap"
trigger = { Key = 17 }
at = { x = 0.5, y = 0.5 }
[bindings.b]
type = "tap"
trigger = { Key = 17 }
at = { x = 0.6, y = 0.6 }
"#).unwrap();
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert!(s.is_empty());
        assert_eq!(s.problems.len(), 1);
        assert!(s.problems[0].error.contains("duplicate trigger"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two profiles at the same precedence claiming one package: deterministic
    /// choice plus an explicit warning. Choosing silently is unacceptable.
    #[test]
    fn same_package_collision_is_reported_deterministically() {
        let d = tmp("coll");
        write(&d, "a-wasd.toml", "com.example.game", 17);
        write(&d, "b-arrows.toml", "com.example.game", 103);
        let s1 = Store::from_dirs(&[(d.clone(), Origin::System)]);
        let s2 = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert_eq!(s1.len(), 1);
        assert_eq!(s1.problems.len(), 1, "the collision must be reported");
        assert_eq!(
            s1.for_package("com.example.game").unwrap().path,
            s2.for_package("com.example.game").unwrap().path,
            "the choice must be reproducible"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let d = tmp("ext");
        write(&d, "g.toml", "com.example.g", 17);
        std::fs::write(d.join("notes.txt"), "not a profile").unwrap();
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert_eq!(s.len(), 1);
        assert!(s.problems.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Default search paths must NOT depend on the working directory.
    #[test]
    fn default_dirs_never_depend_on_cwd() {
        let dirs = default_dirs();
        let cwd = std::env::current_dir().unwrap();
        for (d, _) in &dirs {
            assert_ne!(d, &cwd.join("profiles"),
                "search path must not depend on cwd: {}", d.display());
        }
    }

    #[test]
    fn user_dir_has_highest_priority_in_defaults() {
        let dirs = default_dirs();
        if let Some(u) = user_profile_dir() {
            assert_eq!(dirs.first().map(|(p, _)| p.clone()), Some(u),
                "the user directory must come first");
        }
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let s = Store::from_dirs(&[(PathBuf::from("/olmayan/dizin"), Origin::System)]);
        assert!(s.is_empty());
        assert!(s.problems.is_empty());
    }
}
