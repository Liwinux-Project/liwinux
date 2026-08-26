//! Profil deposu: profilleri bulur, yükler ve pakete göre eşler.
//!
//! # Öncelik
//!
//! Kullanıcı profilleri sistem profillerini **gölgeler**. Böylece kullanıcı
//! dağıtımla gelen bir profili düzenlemek için onu kopyalar; güncelleme
//! geldiğinde kendi değişikliği kaybolmaz.
//!
//! ```text
//! $XDG_CONFIG_HOME/liwinux/profiles/   (kullanıcı — kazanır)
//! /usr/share/liwinux/profiles/         (sistem)
//! ./profiles/                          (geliştirme)
//! ```

use super::profile::{Profile, ProfileError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Bir profilin nereden geldiği. Kullanıcıya "hangi dosya çalışıyor"
/// sorusunu cevaplayabilmek için taşınır.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// En yüksek öncelik.
    User,
    System,
    /// Depo içinden; yalnızca geliştirme sırasında.
    Bundled,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub profile: Profile,
    pub path: PathBuf,
    pub origin: Origin,
}

/// Yüklenemeyen profil. Sessizce yutmuyoruz: bozuk bir dosya
/// kullanıcıya bildirilmeli, yoksa "profilim neden çalışmıyor" sorusu
/// cevapsız kalır.
#[derive(Debug)]
pub struct BadProfile {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Store {
    /// paket adı -> en yüksek öncelikli girdi
    by_package: BTreeMap<String, Entry>,
    pub problems: Vec<BadProfile>,
}

impl Store {
    /// Varsayılan dizinleri tarar.
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
                        // Daha yüksek öncelikli (küçük Origin) kazanır.
                        Some(prev) if prev.origin < entry.origin => {}
                        // AYNI öncelikte iki profil aynı paketi iddia ediyor.
                        // Sessizce birini seçmek belirlenimsiz davranış üretir:
                        // hangisinin yükleneceği dizin okuma sırasına kalır ve
                        // kullanıcı "bazen eski profilim çalışıyor" der.
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
                                     '{}' kullanılıyor. Birini silin veya paket adını değiştirin.",
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

/// Aranacak dizinler, öncelik sırasıyla.
///
/// ÇALIŞMA DİZİNİNE BAKILMAZ. Bakılırsa aynı komut farklı dizinlerden
/// farklı davranır ve kullanıcı "profilim bazen bulunuyor" der — teşhisi
/// zor, açıklaması utandırıcı bir hata sınıfı.
pub fn default_dirs() -> Vec<(PathBuf, Origin)> {
    let mut dirs: Vec<(PathBuf, Origin)> = Vec::new();
    if let Some(cfg) = user_profile_dir() {
        dirs.push((cfg, Origin::User));
    }
    dirs.push((PathBuf::from("/usr/share/liwinux/profiles"), Origin::System));
    // Geliştirme kolaylığı: çalıştırılabilir dosyanın yanındaki depo.
    // Çalışma dizini DEĞİL, binary'nin konumu esas alınır.
    if let Some(d) = bundled_dir() {
        dirs.push((d, Origin::Bundled));
    }
    if let Some(env) = std::env::var_os("LIWINUX_PROFILE_DIR") {
        dirs.push((PathBuf::from(env), Origin::User));
    }
    dirs
}

/// `target/{debug,release}/liw` -> depo kökündeki `profiles/`.
fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    // En fazla dört seviye yukarı bak; daha fazlası alakasız dizinlere sapar.
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
        assert!(s.for_package("com.yok").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Kullanıcı profili sistem profilini gölgelemeli.
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

    /// Tarama sırası öncelikten bağımsız olmalı.
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

    /// Bozuk profil DİĞERLERİNİ gizlememeli ve sessizce yutulmamalı.
    #[test]
    fn broken_profile_is_reported_and_others_still_load() {
        let d = tmp("bad");
        write(&d, "iyi.toml", "com.example.iyi", 17);
        std::fs::write(d.join("bozuk.toml"), "bu geçerli toml değil [[[").unwrap();
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert!(s.for_package("com.example.iyi").is_some(), "iyi profil yüklenmeli");
        assert_eq!(s.problems.len(), 1, "bozuk profil raporlanmalı");
        assert!(s.problems[0].path.ends_with("bozuk.toml"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Doğrulamadan geçemeyen profil de sorun olarak bildirilmeli.
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
        assert!(s.problems[0].error.contains("aynı tetikleyici"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Aynı öncelikte iki profil aynı paketi iddia ederse: belirlenimli
    /// seçim + açık uyarı. Sessizce birini seçmek kabul edilemez.
    #[test]
    fn same_package_collision_is_reported_deterministically() {
        let d = tmp("coll");
        write(&d, "a-wasd.toml", "com.example.game", 17);
        write(&d, "b-arrows.toml", "com.example.game", 103);
        let s1 = Store::from_dirs(&[(d.clone(), Origin::System)]);
        let s2 = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert_eq!(s1.len(), 1);
        assert_eq!(s1.problems.len(), 1, "çakışma bildirilmeli");
        assert_eq!(
            s1.for_package("com.example.game").unwrap().path,
            s2.for_package("com.example.game").unwrap().path,
            "seçim tekrarlanabilir olmalı"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let d = tmp("ext");
        write(&d, "g.toml", "com.example.g", 17);
        std::fs::write(d.join("notlar.txt"), "bu profil değil").unwrap();
        let s = Store::from_dirs(&[(d.clone(), Origin::System)]);
        assert_eq!(s.len(), 1);
        assert!(s.problems.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Varsayılan arama yolları çalışma dizinine BAĞLI OLMAMALI.
    #[test]
    fn default_dirs_never_depend_on_cwd() {
        let dirs = default_dirs();
        let cwd = std::env::current_dir().unwrap();
        for (d, _) in &dirs {
            assert_ne!(d, &cwd.join("profiles"),
                "arama yolu çalışma dizinine bağlı olmamalı: {}", d.display());
        }
    }

    #[test]
    fn user_dir_has_highest_priority_in_defaults() {
        let dirs = default_dirs();
        if let Some(u) = user_profile_dir() {
            assert_eq!(dirs.first().map(|(p, _)| p.clone()), Some(u),
                "kullanıcı dizini ilk sırada olmalı");
        }
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let s = Store::from_dirs(&[(PathBuf::from("/olmayan/dizin"), Origin::System)]);
        assert!(s.is_empty());
        assert!(s.problems.is_empty());
    }
}
