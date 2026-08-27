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

/// Depoyla gelen profilleri kullanıcı dizinine kopyalar.
///
/// Böylece profiller çalıştırma dizininden bağımsız bulunur ve kullanıcı
/// bunları serbestçe düzenleyebilir.
pub fn install(force: bool, from: Option<std::path::PathBuf>) -> Result<()> {
    let dest = liw_core::input::store::user_profile_dir()
        .context("kullanıcı yapılandırma dizini belirlenemedi")?;
    std::fs::create_dir_all(&dest)
        .with_context(|| format!("dizin oluşturulamadı: {}", dest.display()))?;

    // Kaynak: açık --from > çalıştırılabilirin yanındaki depo.
    // Kurulu binary'nin (~/.local/bin) yanında depo olmayacağı için
    // --from çoğu zaman gerekli olur; bunu hata mesajında söylüyoruz.
    let src = match from {
        Some(p) => p,
        None => liw_core::input::store::default_dirs()
            .into_iter()
            .find(|(_, o)| *o == Origin::Bundled)
            .map(|(p, _)| p)
            .context("depo profilleri bulunamadı — kaynak dizini --from ile verin \
                      (örn: --from ~/Projects/liwinux/profiles)")?,
    };
    println!("Kaynak: {}", src.display());

    let mut copied = 0;
    let mut skipped = 0;
    for e in std::fs::read_dir(&src).with_context(|| format!("okunamadı: {}", src.display()))? {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "toml") { continue; }
        let name = p.file_name().context("dosya adı yok")?;
        let target = dest.join(name);
        if target.exists() && !force {
            println!("  atlandı (zaten var): {}", name.to_string_lossy());
            skipped += 1;
            continue;
        }
        // Üzerine yazmadan ÖNCE yedekle.
        //
        // Kullanıcı profilleri düzenleyiciyle ayarlanmış koordinatlar
        // içerir; --force onları geri dönüşsüz siliyordu. Gerçekte oldu:
        // saatlerce ayarlanmış bir FPS profili tek komutla kayboldu.
        if target.exists() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs()).unwrap_or(0);
            let bak = target.with_extension(format!("toml.bak-{stamp}"));
            std::fs::copy(&target, &bak)
                .with_context(|| format!("yedeklenemedi: {}", target.display()))?;
            println!("  yedek: {}", bak.file_name().unwrap_or_default().to_string_lossy());
        }
        std::fs::copy(&p, &target)
            .with_context(|| format!("kopyalanamadı: {}", p.display()))?;
        println!("  kuruldu: {}", name.to_string_lossy());
        copied += 1;
    }
    println!();
    println!("{copied} profil kuruldu, {skipped} atlandı -> {}", dest.display());
    if skipped > 0 { println!("Üzerine yazmak için: liw profile install --force"); }
    Ok(())
}

/// Bir bağlantının koordinatını değiştirir.
///
/// `toml_edit` kullanılıyor: normal serileştirme profildeki YORUMLARI ve
/// biçimi siler. Profiller elle okunup düzenlenen dosyalar; onları bozmak
/// kullanıcının işini zorlaştırır.
pub fn set_coord(package: &str, binding: &str, field: &str, x: f64, y: f64) -> Result<()> {
    let s = Store::discover();
    let entry = s.for_package(package)
        .with_context(|| format!("'{package}' için profil yok"))?;
    let path = entry.path.clone();

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("okunamadı: {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text.parse()
        .with_context(|| format!("TOML ayrıştırılamadı: {}", path.display()))?;

    let b = doc.get_mut("bindings")
        .and_then(|t| t.get_mut(binding))
        .with_context(|| format!("'{binding}' adlı bağlantı yok"))?;
    let target = b.get_mut(field)
        .with_context(|| format!("'{binding}' bağlantısında '{field}' alanı yok"))?;

    let mut tbl = toml_edit::InlineTable::new();
    tbl.insert("x", toml_edit::value(x).into_value().unwrap());
    tbl.insert("y", toml_edit::value(y).into_value().unwrap());
    *target = toml_edit::Item::Value(toml_edit::Value::InlineTable(tbl));

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("yazılamadı: {}", path.display()))?;
    println!("{binding}.{field} = ({x:.3}, {y:.3})");
    println!("dosya: {}", path.display());
    println!();
    println!("Değişikliğin etkili olması için keymapper yeniden başlatılmalı:");
    println!("  liw keymap stop && liw keymap start --grab");
    Ok(())
}

/// Bir bağlantının koordinatına dokunur — yerleşimi görsel doğrulamak için.
pub async fn poke_binding(package: &str, binding: &str, delay_s: u64) -> Result<()> {
    use liw_core::input::Binding;
    let s = Store::discover();
    let entry = s.for_package(package)
        .with_context(|| format!("'{package}' için profil yok"))?;
    let b = entry.profile.bindings.get(binding)
        .with_context(|| format!("'{binding}' adlı bağlantı yok"))?;

    let (x, y, to) = match b {
        Binding::Tap { at, .. } | Binding::Toggle { at, .. } => (at.x, at.y, None),
        Binding::Aim { origin, .. } => (origin.x, origin.y, None),
        Binding::Joystick { center, radius, .. } =>
            // Joystick'te merkezden yukarı doğru sürükle: hem merkezi hem
            // yarıçapı aynı anda görürsün.
            (center.x, center.y, Some((center.x, center.y - radius))),
        Binding::Swipe { from, to, .. } => (from.x, from.y, Some((to.x, to.y))),
    };
    println!("{binding}: ({x:.3}, {y:.3})");
    crate::keymap::poke(x, y, 250, to.map(|(a, b)| (a, b)),
                        liw_core::input::ScreenMap::default(), delay_s, false).await
}
