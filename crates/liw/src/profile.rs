//! `liw profile` — profile store commands.

use anyhow::{Context, Result};
use liw_core::input::store::{Origin, Store};

fn origin_label(o: Origin) -> &'static str {
    match o {
        Origin::User => "user",
        Origin::System => "sistem",
        Origin::Bundled => "depo",
    }
}

fn report_problems(s: &Store) {
    if s.problems.is_empty() { return; }
    eprintln!();
    eprintln!("PROFILES THAT FAILED TO LOAD ({}):", s.problems.len());
    for p in &s.problems {
        eprintln!("  {}", p.path.display());
        eprintln!("    {}", p.error);
    }
}

pub fn list() -> Result<()> {
    let s = Store::discover();
    if s.is_empty() {
        println!("No profiles found.");
        println!("Aranan yerler:");
        if let Some(d) = liw_core::input::store::user_profile_dir() {
            println!("  {}", d.display());
        }
        println!("  /usr/share/liwinux/profiles");
        println!("  ./profiles");
        report_problems(&s);
        return Ok(());
    }
    println!("{:<34} {:<10} {:<5} {}", "PACKAGE", "SOURCE", "BIND", "NAME");
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
        .with_context(|| format!("no profile for '{package}' — check with 'liw profile list'"))?;
    println!("Ad     : {}", e.profile.name);
    println!("Paket  : {}", e.profile.package);
    println!("Kaynak : {} ({})", origin_label(e.origin), e.path.display());
    println!();
    println!("{:<14} {:<10} {}", "BINDING", "TYPE", "DETAIL");
    for (name, b) in &e.profile.bindings {
        use liw_core::input::Binding::*;
        let (kind, detail) = match b {
            Tap { at, .. } => ("tap", format!("({:.2}, {:.2})", at.x, at.y)),
            Toggle { at, .. } => ("toggle", format!("({:.2}, {:.2})", at.x, at.y)),
            Joystick { center, radius, .. } =>
                ("joystick", format!("merkez ({:.2}, {:.2}) r={radius:.2}", center.x, center.y)),
            Aim { sensitivity, deadzone, .. } =>
                ("aim", format!("sensitivity={sensitivity} deadzone={deadzone}")),
            Swipe { from, to, duration_ms, .. } =>
                ("swipe", format!("({:.2},{:.2}) -> ({:.2},{:.2})  {duration_ms}ms",
                                  from.x, from.y, to.x, to.y)),
        };
        println!("{:<14} {:<10} {}", name, kind, detail);
    }
    Ok(())
}

/// Reports which profile applies to the foreground app.
pub async fn which() -> Result<()> {
    let h = liw_core::HelperClient::connect().await
        .context("could not connect to liwd-helper — is it running? \
                  (systemctl status liwd-helper)")?;
    let pkg = h.foreground_package().await.context("could not query the foreground")?;
    if pkg.is_empty() {
        println!("Could not determine the foreground app.");
        println!("Is Waydroid open with an app in the foreground?");
        return Ok(());
    }
    println!("Foreground: {pkg}");
    let s = Store::discover();
    match s.for_package(&pkg) {
        Some(e) => {
            println!("Profil  : {} ({})", e.profile.name, origin_label(e.origin));
            println!("Dosya   : {}", e.path.display());
        }
        None => {
            println!("Profile   : NONE");
            println!();
            println!("To create a profile for this game:");
            if let Some(d) = liw_core::input::store::user_profile_dir() {
                println!("  {}/{}.toml", d.display(), pkg);
            }
        }
    }
    Ok(())
}

/// Copies the profiles shipped with the repository into the user directory.
///
/// This makes profiles discoverable independently of the working directory and
/// lets the user edit them freely.
pub fn install(force: bool, from: Option<std::path::PathBuf>) -> Result<()> {
    let dest = liw_core::input::store::user_profile_dir()
        .context("could not determine the user config directory")?;
    std::fs::create_dir_all(&dest)
        .with_context(|| format!("could not create directory: {}", dest.display()))?;

    // Source: explicit --from > the repository next to the executable.
    // An installed binary (~/.local/bin) has no repository beside it, so
    // --from is usually required; the error message says so.
    let src = match from {
        Some(p) => p,
        None => liw_core::input::store::default_dirs()
            .into_iter()
            .find(|(_, o)| *o == Origin::Bundled)
            .map(|(p, _)| p)
            .context("repository profiles not found — pass the source directory with --from \
                      (e.g. --from ~/Projects/liwinux/profiles)")?,
    };
    println!("Kaynak: {}", src.display());

    let mut copied = 0;
    let mut skipped = 0;
    for e in std::fs::read_dir(&src).with_context(|| format!("could not read: {}", src.display()))? {
        let p = e?.path();
        if p.extension().is_none_or(|x| x != "toml") { continue; }
        let name = p.file_name().context("no file name")?;
        let target = dest.join(name);
        if target.exists() && !force {
            println!("  skipped (already present): {}", name.to_string_lossy());
            skipped += 1;
            continue;
        }
        // Back up BEFORE overwriting.
        //
        // User profiles hold coordinates tuned with the editor; --force used to
        // delete them irreversibly. It actually happened: an FPS profile tuned
        // over hours vanished with a single command.
        if target.exists() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs()).unwrap_or(0);
            let bak = target.with_extension(format!("toml.bak-{stamp}"));
            std::fs::copy(&target, &bak)
                .with_context(|| format!("yedeklenemedi: {}", target.display()))?;
            println!("  backup: {}", bak.file_name().unwrap_or_default().to_string_lossy());
        }
        std::fs::copy(&p, &target)
            .with_context(|| format!("could not copy: {}", p.display()))?;
        println!("  kuruldu: {}", name.to_string_lossy());
        copied += 1;
    }
    println!();
    println!("{copied} profiles installed, {skipped} skipped -> {}", dest.display());
    if skipped > 0 { println!("To overwrite: liw profile install --force"); }
    Ok(())
}

/// Changes a binding's coordinate.
///
/// Uses `toml_edit`: ordinary serialization strips the COMMENTS and formatting
/// from the profile. Profiles are files people read and edit by hand; mangling
/// them makes the user's job harder.
pub fn set_coord(package: &str, binding: &str, field: &str, x: f64, y: f64) -> Result<()> {
    let s = Store::discover();
    let entry = s.for_package(package)
        .with_context(|| format!("no profile for '{package}'"))?;
    let path = entry.path.clone();

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read: {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text.parse()
        .with_context(|| format!("could not parse TOML: {}", path.display()))?;

    let b = doc.get_mut("bindings")
        .and_then(|t| t.get_mut(binding))
        .with_context(|| format!("no binding named '{binding}'"))?;
    let target = b.get_mut(field)
        .with_context(|| format!("binding '{binding}' has no field '{field}'"))?;

    let mut tbl = toml_edit::InlineTable::new();
    tbl.insert("x", toml_edit::value(x).into_value().unwrap());
    tbl.insert("y", toml_edit::value(y).into_value().unwrap());
    *target = toml_edit::Item::Value(toml_edit::Value::InlineTable(tbl));

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("could not write: {}", path.display()))?;
    println!("{binding}.{field} = ({x:.3}, {y:.3})");
    println!("dosya: {}", path.display());
    println!();
    println!("Restart the keymapper for the change to take effect:");
    println!("  liw keymap stop && liw keymap start --grab");
    Ok(())
}

/// Pokes a binding's coordinate — to verify placement visually.
pub async fn poke_binding(package: &str, binding: &str, delay_s: u64) -> Result<()> {
    use liw_core::input::Binding;
    let s = Store::discover();
    let entry = s.for_package(package)
        .with_context(|| format!("no profile for '{package}'"))?;
    let b = entry.profile.bindings.get(binding)
        .with_context(|| format!("no binding named '{binding}'"))?;

    let (x, y, to) = match b {
        Binding::Tap { at, .. } | Binding::Toggle { at, .. } => (at.x, at.y, None),
        Binding::Aim { origin, .. } => (origin.x, origin.y, None),
        Binding::Joystick { center, radius, .. } =>
            // On a joystick, drag upward from the centre: you see both the
            // centre and the radius at once.
            (center.x, center.y, Some((center.x, center.y - radius))),
        Binding::Swipe { from, to, .. } => (from.x, from.y, Some((to.x, to.y))),
    };
    println!("{binding}: ({x:.3}, {y:.3})");
    crate::keymap::poke(x, y, 250, to.map(|(a, b)| (a, b)),
                        liw_core::input::ScreenMap::default(), delay_s, false).await
}
