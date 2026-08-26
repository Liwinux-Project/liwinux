//! `liw profile` — profil deposu komutları.

use anyhow::{Context, Result};
use liw_core::input::store::{Origin, Store};

fn origin_label(o: Origin) -> &'static str {
    match o {
        Origin::User => "kullanıcı",
        Origin::System => "sistem",
        Origin::Bundled => "depo",
    }
}

fn report_problems(s: &Store) {
    if s.problems.is_empty() { return; }
    eprintln!();
    eprintln!("YÜKLENEMEYEN PROFİLLER ({}):", s.problems.len());
    for p in &s.problems {
        eprintln!("  {}", p.path.display());
        eprintln!("    {}", p.error);
    }
}

pub fn list() -> Result<()> {
    let s = Store::discover();
    if s.is_empty() {
        println!("Profil bulunamadı.");
        println!("Aranan yerler:");
        if let Some(d) = liw_core::input::store::user_profile_dir() {
            println!("  {}", d.display());
        }
        println!("  /usr/share/liwinux/profiles");
        println!("  ./profiles");
        report_problems(&s);
        return Ok(());
    }
    println!("{:<34} {:<10} {:<5} {}", "PAKET", "KAYNAK", "BAĞ", "AD");
    for e in s.entries() {
        println!("{:<34} {:<10} {:<5} {}",
            e.profile.package, origin_label(e.origin),
            e.profile.bindings.len(), e.profile.name);
    }
    report_problems(&s);
    Ok(())
}

pub fn show(package: &str) -> Result<()> {
    let s = Store::discover();
    let e = s.for_package(package)
        .with_context(|| format!("'{package}' için profil yok — 'liw profile list' ile bak"))?;
    println!("Ad     : {}", e.profile.name);
    println!("Paket  : {}", e.profile.package);
    println!("Kaynak : {} ({})", origin_label(e.origin), e.path.display());
    println!();
    println!("{:<14} {:<10} {}", "BAĞLANTI", "TÜR", "AYRINTI");
    for (name, b) in &e.profile.bindings {
        use liw_core::input::Binding::*;
        let (kind, detail) = match b {
            Tap { at, .. } => ("tap", format!("({:.2}, {:.2})", at.x, at.y)),
            Toggle { at, .. } => ("toggle", format!("({:.2}, {:.2})", at.x, at.y)),
            Joystick { center, radius, .. } =>
                ("joystick", format!("merkez ({:.2}, {:.2}) r={radius:.2}", center.x, center.y)),
            Aim { sensitivity, deadzone, .. } =>
                ("aim", format!("hassasiyet={sensitivity} ölü bölge={deadzone}")),
            Swipe { from, to, duration_ms, .. } =>
                ("swipe", format!("({:.2},{:.2}) -> ({:.2},{:.2})  {duration_ms}ms",
                                  from.x, from.y, to.x, to.y)),
        };
        println!("{:<14} {:<10} {}", name, kind, detail);
    }
    Ok(())
}

/// Ön plandaki uygulama için hangi profilin geçerli olduğunu söyler.
pub async fn which() -> Result<()> {
    let h = liw_core::HelperClient::connect().await
        .context("liwd-helper'a bağlanılamadı — çalışıyor mu? \
                  (systemctl status liwd-helper)")?;
    let pkg = h.foreground_package().await.context("ön plan sorgulanamadı")?;
    if pkg.is_empty() {
        println!("Ön plandaki uygulama tespit edilemedi.");
        println!("Waydroid açık ve bir uygulama ön planda mı?");
        return Ok(());
    }
    println!("Ön plan : {pkg}");
    let s = Store::discover();
    match s.for_package(&pkg) {
        Some(e) => {
            println!("Profil  : {} ({})", e.profile.name, origin_label(e.origin));
            println!("Dosya   : {}", e.path.display());
        }
        None => {
            println!("Profil  : YOK");
            println!();
            println!("Bu oyun için profil oluşturmak istersen:");
            if let Some(d) = liw_core::input::store::user_profile_dir() {
                println!("  {}/{}.toml", d.display(), pkg);
            }
        }
    }
    Ok(())
}
