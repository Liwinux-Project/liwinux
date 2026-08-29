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

// ---------------------------------------------------------------------------
// Writing
//
// This used to live in the browser editor inside `liw`. A UI cannot reach it
// there, and reimplementing it would mean a second, diverging writer for the
// same files. It belongs next to the reader.
// ---------------------------------------------------------------------------

/// Renders a profile back into TOML, PRESERVING the existing file's comments.
///
/// Pure on purpose: comment preservation is the part that breaks silently, and
/// it can only be pinned down by tests that feed text in and read text out.
///
/// `existing` is the current file, when there is one. Without it a fresh
/// document is produced.
pub fn render(existing: Option<&str>, p: &Profile) -> Result<String, ProfileError> {
    // Validate BEFORE writing. The worst case is the same trigger bound twice:
    // the engine cannot know which to pick and the user experiences it as
    // "sometimes it works".
    p.validate()?;

    let mut doc: toml_edit::DocumentMut = match existing {
        Some(t) => t.parse().map_err(|e| ProfileError::Edit(format!("{e}")))?,
        None => toml_edit::DocumentMut::new(),
    };
    set_str(&mut doc, "name", &p.name);
    set_str(&mut doc, "package", &p.package);

    if doc.get("bindings").is_none() {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("bindings", toml_edit::Item::Table(t));
    }
    let tbl = doc.get_mut("bindings").and_then(|i| i.as_table_mut())
        .ok_or_else(|| ProfileError::Edit("[bindings] is not a table".into()))?;

    // Removed bindings.
    for k in tbl.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>() {
        if !p.bindings.contains_key(&k) { tbl.remove(&k); }
    }

    for (name, b) in &p.bindings {
        let fresh = toml_edit::ser::to_document(b)
            .map_err(|e| ProfileError::Edit(format!("'{name}': {e}")))?;
        let fresh = fresh.as_table();
        match tbl.get_mut(name).and_then(|i| i.as_table_like_mut()) {
            Some(old) => {
                for (k, v) in fresh.iter() {
                    let mut v = v.clone();
                    normalise_floats(&mut v);
                    match old.get_mut(k) {
                        Some(slot) => {
                            // Keep the END-OF-LINE comment.
                            //
                            // The "# W" in `up = { Key = 17 }   # W` lives in
                            // the value's suffix decor; replacing the value
                            // outright deleted it. Those comments are the ONLY
                            // place saying which letter a key code is, so
                            // losing them is silent and permanent.
                            let keep = slot.as_value().map(|x| x.decor().clone());
                            *slot = v;
                            if let (Some(d), Some(nv)) = (keep, slot.as_value_mut()) {
                                *nv.decor_mut() = d;
                            }
                        }
                        None => { old.insert(k, v); }
                    }
                }
                // If the type changed, keys of the old type must not remain.
                for k in old.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>() {
                    if fresh.get(&k).is_none() { old.remove(&k); }
                }
            }
            None => {
                let mut t = fresh.clone();
                for (_, v) in t.iter_mut() { normalise_floats(v); }
                t.set_implicit(false);
                tbl.insert(name, toml_edit::Item::Table(t));
            }
        }
    }
    Ok(doc.to_string())
}

/// Sets a top-level string, keeping any decor (and so any comment).
fn set_str(doc: &mut toml_edit::DocumentMut, key: &str, val: &str) {
    match doc.get_mut(key).and_then(|i| i.as_value_mut()) {
        Some(slot) => {
            let d = slot.decor().clone();
            *slot = toml_edit::Value::from(val);
            *slot.decor_mut() = d;
        }
        None => { doc.insert(key, toml_edit::value(val)); }
    }
}

/// Shortens f64 that came from f32.
///
/// serde widens `0.148f32` to `0.14800000190734863`. Writing that back makes
/// every save churn the file and turns a hand-written profile into noise.
fn normalise_floats(item: &mut toml_edit::Item) {
    fn fix(v: &mut toml_edit::Value) {
        match v {
            toml_edit::Value::Float(f) => {
                let short: f64 = (*f.value() as f32).to_string().parse()
                    .unwrap_or(*f.value());
                let decor = f.decor().clone();
                *v = toml_edit::Value::from(short);
                *v.decor_mut() = decor;
            }
            toml_edit::Value::InlineTable(t) => {
                for (_, iv) in t.iter_mut() { fix(iv); }
            }
            toml_edit::Value::Array(a) => { for iv in a.iter_mut() { fix(iv); } }
            _ => {}
        }
    }
    match item {
        toml_edit::Item::Value(v) => fix(v),
        toml_edit::Item::Table(t) => { for (_, i) in t.iter_mut() { normalise_floats(i); } }
        _ => {}
    }
}

impl Store {
    /// The file a save for this package should land in.
    ///
    /// A user profile is written in place. A system or bundled one is NOT:
    /// it is shadowed by a new user file, so a package update does not
    /// silently discard the user's edits (and does not get overwritten by
    /// them either).
    pub fn save_target(&self, pkg: &str) -> Option<PathBuf> {
        match self.by_package.get(pkg) {
            Some(e) if e.origin == Origin::User => Some(e.path.clone()),
            _ => Some(user_profile_dir()?.join(format!("{pkg}.toml"))),
        }
    }

    /// Writes a profile, preserving comments. Returns the file written.
    ///
    /// When shadowing a system profile for the first time, the system file's
    /// TEXT is used as the base so its comments come along — they are usually
    /// the only explanation of what each coordinate means.
    pub fn save(&self, p: &Profile) -> Result<PathBuf, ProfileError> {
        let target = self.save_target(&p.package).ok_or_else(|| {
            ProfileError::Edit("no user profile directory (is HOME set?)".into())
        })?;
        let base = match std::fs::read_to_string(&target) {
            Ok(t) => Some(t),
            // No user file yet: seed from the profile we are shadowing.
            Err(_) => self.by_package.get(&p.package)
                .and_then(|e| std::fs::read_to_string(&e.path).ok()),
        };
        let text = render(base.as_deref(), p)?;
        if let Some(dir) = target.parent() { std::fs::create_dir_all(dir)?; }
        std::fs::write(&target, text)?;
        Ok(target)
    }

    /// Deletes the USER profile for a package. Returns the file removed.
    ///
    /// System and bundled profiles are never touched: they are not ours to
    /// delete, and removing one would come back on the next update anyway.
    pub fn delete(&self, pkg: &str) -> Result<PathBuf, ProfileError> {
        match self.by_package.get(pkg) {
            Some(e) if e.origin == Origin::User => {
                std::fs::remove_file(&e.path)?;
                Ok(e.path.clone())
            }
            Some(_) => Err(ProfileError::Invalid(format!(
                "'{pkg}' is not a user profile; only user profiles can be deleted"))),
            None => Err(ProfileError::Invalid(format!("no profile for '{pkg}'"))),
        }
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::profile::{Binding, Trigger};
    use crate::touch::Norm;

    fn sample() -> Profile {
        let mut b = BTreeMap::new();
        b.insert("jump".into(), Binding::Tap {
            trigger: Trigger::Key(57), at: Norm::new(0.9, 0.8) });
        Profile { name: "T".into(), package: "com.x".into(), bindings: b }
    }

    /// Comments are the only place saying what a coordinate means. Losing
    /// them on save is silent and permanent.
    #[test]
    fn comments_survive_a_round_trip() {
        let src = "# top of file\nname = \"T\"\npackage = \"com.x\"\n\n\
                   # the jump button\n[bindings.jump]\ntype = \"tap\"\n\
                   trigger = { Key = 57 }   # SPACE\n\
                   at = { x = 0.9, y = 0.8 }\n";
        let out = render(Some(src), &sample()).unwrap();
        assert!(out.contains("# top of file"), "{out}");
        assert!(out.contains("# the jump button"), "{out}");
        assert!(out.contains("# SPACE"), "end-of-line comment lost:\n{out}");
    }

    /// f32 -> f64 widening must not turn 0.148 into 0.14800000190734863.
    #[test]
    fn floats_do_not_grow_noise() {
        let mut b = BTreeMap::new();
        b.insert("move".into(), Binding::Joystick {
            up: Trigger::Key(17), down: Trigger::Key(31),
            left: Trigger::Key(30), right: Trigger::Key(32),
            center: Norm::new(0.148, 0.738), radius: 0.085 });
        let p = Profile { name: "T".into(), package: "com.x".into(), bindings: b };
        let out = render(None, &p).unwrap();
        assert!(out.contains("0.148"), "{out}");
        assert!(!out.contains("0.1480000"), "float noise:\n{out}");
    }

    /// A removed binding must actually leave the file.
    #[test]
    fn deleted_bindings_are_removed() {
        let src = "name = \"T\"\npackage = \"com.x\"\n\
                   [bindings.jump]\ntype = \"tap\"\ntrigger = { Key = 57 }\n\
                   at = { x = 0.9, y = 0.8 }\n\
                   [bindings.gone]\ntype = \"tap\"\ntrigger = { Key = 30 }\n\
                   at = { x = 0.1, y = 0.1 }\n";
        let out = render(Some(src), &sample()).unwrap();
        assert!(!out.contains("bindings.gone"), "{out}");
        assert!(out.contains("bindings.jump"), "{out}");
    }

    /// Changing a binding's type must not leave the old type's keys behind;
    /// the file would no longer parse.
    #[test]
    fn switching_type_drops_the_old_keys() {
        let src = "name = \"T\"\npackage = \"com.x\"\n\
                   [bindings.jump]\ntype = \"swipe\"\ntrigger = { Key = 57 }\n\
                   from = { x = 0.5, y = 0.5 }\nto = { x = 0.5, y = 0.2 }\n\
                   duration_ms = 45\n";
        let out = render(Some(src), &sample()).unwrap();
        assert!(!out.contains("duration_ms"), "stale key left:\n{out}");
        let back = Profile::from_toml(&out).expect("must still parse");
        assert!(matches!(back.bindings.get("jump"), Some(Binding::Tap { .. })));
    }

    /// An invalid profile must be refused BEFORE it reaches the disk.
    #[test]
    fn duplicate_trigger_is_refused() {
        let mut p = sample();
        p.bindings.insert("other".into(), Binding::Tap {
            trigger: Trigger::Key(57), at: Norm::new(0.1, 0.1) });
        assert!(render(None, &p).is_err());
    }

    /// A brand-new file must be complete enough to load again.
    #[test]
    fn a_fresh_document_round_trips() {
        let out = render(None, &sample()).unwrap();
        let back = Profile::from_toml(&out).unwrap();
        assert_eq!(back.package, "com.x");
        assert_eq!(back.bindings.len(), 1);
    }
}
