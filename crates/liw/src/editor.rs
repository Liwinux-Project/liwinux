//! `liw profile edit` — the visual profile editor.
//!
//! Takes a screenshot of the game, opens a local HTTP server and lets you edit
//! coordinates by dragging markers in the browser.
//!
//! # Neden web
//!
//! It gives real visual editing without adding a GUI library dependency. The
//! server binds to 127.0.0.1 only and edits a single profile.
//!
//! # Accuracy
//!
//! The editor's only job is producing coordinates, and being a few pixels off
//! means missing the button in the game. Therefore:
//!
//! * The browser is told the REAL pixel size of the screenshot; the UI scales
//!   the image to fit but always converts coordinates back to source pixels.
//! * Four decimals are written: at 2560 pixels 0.0001 ~ 0.26 pixel, so rounding
//!   cannot cause a visible shift. (It used to be three decimals and could
//!   drift by up to 2.5 pixels.)
//! * The profile is VALIDATED before saving — giving the same key to two
//!   bindings produces a "sometimes it works" bug that is very hard to spot.

use anyhow::{bail, Context, Result};
use liw_core::input::{Binding, Profile, Store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// All the state the browser sees.
#[derive(serde::Serialize)]
struct State {
    name: String,
    package: String,
    /// Real pixel size of the screenshot. The UI's scaling maths rests entirely
    /// on this.
    width: u32,
    height: u32,
    /// Image version: incremented on recapture, busting the browser cache.
    shot: u64,
    bindings: BTreeMap<String, Binding>,
}

/// A single touch request from the browser.
#[derive(serde::Deserialize)]
struct Poke {
    x: f32,
    y: f32,
    #[serde(default)]
    to: Option<[f32; 2]>,
    #[serde(default = "default_hold")]
    hold_ms: u64,
}
fn default_hold() -> u64 { 250 }

struct Editor {
    path: PathBuf,
    name: String,
    package: String,
    bindings: Mutex<BTreeMap<String, Binding>>,
    shot: PathBuf,
    /// (width, height, version)
    shot_info: Mutex<(u32, u32, u64)>,
}

/// Captures an image of the Waydroid window.
///
/// Uses `spectacle -a` (active window). `grim` DOES NOT WORK on KWin: KWin does
/// not support the `wlr-screencopy` protocol.
///
/// The `window_geometry()` call makes the window fullscreen and ACTIVATES it,
/// so `-a` captures exactly the Waydroid window.
async fn screenshot(out: &Path) -> Result<(u32, u32)> {
    let (_x, _y, w, h) = window_geometry().await?;
    // RAISE the window: spectacle -a captures the active window, and the
    // command is triggered from a terminal or browser, which would be active.
    activate_window().await?;
    // Slack for KWin to hand over focus and for the window to be drawn.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let _ = std::fs::remove_file(out);
    let st = tokio::process::Command::new("spectacle")
        .args(["-a", "-b", "-n", "-o", out.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status().await.context("could not run spectacle (is it installed?)")?;
    // spectacle writes in the background; wait for the file to appear.
    for _ in 0..30 {
        if out.is_file() && std::fs::metadata(out).map(|m| m.len() > 0).unwrap_or(false) {
            // Read the REAL size from the PNG header.
            //
            // The geometry KWin reports is in logical pixels; with a display
            // scale other than 1 the image comes out larger. The UI's scaling
            // maths must rest on the image's own size, or every marker shifts
            // by a constant ratio.
            if let Some((pw, ph)) = png_size(out) { return Ok((pw, ph)); }
            return Ok((w, h));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    bail!("no screenshot appeared (spectacle exit: {st})");
}

/// Width/height from the PNG IHDR. No decoding needed; the first 24 bytes do.
fn png_size(p: &Path) -> Option<(u32, u32)> {
    let b = std::fs::read(p).ok()?;
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    let rd = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    Some((rd(16), rd(20)))
}

/// Restarts the keymapper (to load the saved profile).
///
/// Making a saved profile take effect used to require `liw keymap stop && start`;
/// leaving the editor for the terminal breaks the edit-and-try loop. The same
/// D-Bus calls are made here.
///
/// If it is not running we do NOT start it: the user may have turned it off
/// deliberately and having the editor grab devices by itself would be a
/// surprise.
async fn reload_keymapper() -> Result<&'static str> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await
        .context("could not connect to liwd")?;
    let st: String = p.call("KeymapperStatus", &()).await
        .context("could not read the keymapper state")?;
    let running = serde_json::from_str::<serde_json::Value>(&st)
        .map(|v| v["running"].as_bool().unwrap_or(false)).unwrap_or(false);
    if !running { return Ok("the keymapper was already off — left it alone"); }

    let grabbed = serde_json::from_str::<serde_json::Value>(&st)
        .map(|v| v["grabbed"].as_bool().unwrap_or(false)).unwrap_or(false);
    p.call::<_, _, ()>("StopKeymapper", &()).await
        .context("could not stop the keymapper")?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    p.call::<_, _, ()>("StartKeymapper", &(true,)).await
        .context("could not start the keymapper")?;
    Ok(if grabbed { "keymapper restarted" }
       else { "keymapper restarted (the grab is taken in game mode)" })
}

async fn activate_window() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await?;
    p.call::<_, _, ()>("ActivateWindow", &()).await
        .context("could not raise the window")?;
    Ok(())
}

async fn window_geometry() -> Result<(i32, i32, u32, u32)> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await
        .context("could not connect to liwd")?;
    // Request fullscreen first, so the geometry is refreshed.
    let _: Result<bool, _> = p.call("Fullscreen", &()).await;
    let json: String = p.call("WindowGeometry", &()).await
        .context("could not get the window geometry")?;
    let v: serde_json::Value = serde_json::from_str(&json)?;
    if !v["found"].as_bool().unwrap_or(false) {
        bail!("Waydroid window not found — is the game open?");
    }
    Ok((v["x"].as_i64().unwrap_or(0) as i32,
        v["y"].as_i64().unwrap_or(0) as i32,
        v["width"].as_u64().unwrap_or(0) as u32,
        v["height"].as_u64().unwrap_or(0) as u32))
}

/// Reduces decimals to the SHORTEST `f32` representation.
///
/// Profile coordinates are `f32`; TOML serialization goes through `f64` and
/// `0.148` was written to the file as `0.14800000190734863`. The value is
/// correct but the file becomes unreadable and grows a little on every save.
///
/// The `Display` impl of `f32` already produces the shortest round-tripping
/// decimal, so this reduction loses NO precision: the same `f32` is read back.
/// At 2560 pixels `f32` resolution is ~0.0002 pixel.
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

/// Writes the edited bindings back to TOML.
///
/// Updated in place with `toml_edit` INSTEAD of regenerating the file: the
/// comments in profiles are half the documentation in this project and
/// regeneration would delete all of them. Because comments live in the key's
/// decor, changing only the VALUE preserves them.
fn write_back(path: &Path, bindings: &BTreeMap<String, Binding>,
              name: &str, package: &str) -> Result<()> {
    // Validate BEFORE saving. Most important is the same trigger used in two
    // bindings: the engine cannot know which to pick and the user experiences
    // it as "sometimes it works".
    Profile { name: name.into(), package: package.into(), bindings: bindings.clone() }
        .validate().context("invalid profile")?;

    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    if doc.get("bindings").is_none() {
        doc.insert("bindings", toml_edit::Item::Table({
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            t
        }));
    }
    let tbl = doc.get_mut("bindings").and_then(|i| i.as_table_mut())
        .context("[bindings] is not a table")?;

    // Silinenler.
    for k in tbl.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>() {
        if !bindings.contains_key(&k) { tbl.remove(&k); }
    }

    for (bname, b) in bindings {
        let fresh = toml_edit::ser::to_document(b)
            .with_context(|| format!("could not convert '{bname}' to TOML"))?;
        let fresh = fresh.as_table();
        match tbl.get_mut(bname).and_then(|i| i.as_table_like_mut()) {
            Some(old) => {
                // Existing: change only the VALUES, so comments living in the
                // key's decor stay in place.
                for (k, v) in fresh.iter() {
                    let mut v = v.clone();
                    normalise_floats(&mut v);
                    match old.get_mut(k) {
                        Some(slot) => {
                            // SATIR SONU yorumunu koru.
                            //
                            // The "# W" in `up = { Key = 17 }   # W` lives in
                            // the value's suffix decor; replacing the value
                            // outright deleted it. Those comments are the ONLY
                            // place saying which letter a key code is — losing
                            // them is silent and permanent.
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
                tbl.insert(bname, toml_edit::Item::Table(t));
            }
        }
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

pub async fn run(package: &str, port: u16) -> Result<()> {
    let store = Store::discover();
    let entry = store.for_package(package)
        .with_context(|| format!("no profile for '{package}'"))?;

    let shot = std::env::temp_dir().join(format!("liw-edit-{}.png", std::process::id()));
    println!("Taking a screenshot...");
    let (w, h) = screenshot(&shot).await?;
    println!("  {w}x{h} -> {}", shot.display());

    let ed = Arc::new(Editor {
        path: entry.path.clone(),
        name: entry.profile.name.clone(),
        package: entry.profile.package.clone(),
        bindings: Mutex::new(entry.profile.bindings.clone()),
        shot,
        shot_info: Mutex::new((w, h, 1)),
    });

    let listener = TcpListener::bind(("127.0.0.1", port)).await
        .with_context(|| format!("could not open port {port}"))?;
    let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    println!();
    println!("Editor: {url}");
    println!("Opening the browser — press Ctrl+C in this terminal when done.");
    let _ = tokio::process::Command::new("xdg-open").arg(&url)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().await;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { println!(); println!("closed"); break; }
            acc = listener.accept() => {
                let (mut sock, _) = acc?;
                let ed = ed.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(&mut sock, ed).await {
                        tracing_line(&format!("request error: {e}"));
                    }
                });
            }
        }
    }
    let _ = std::fs::remove_file(&ed.shot);
    Ok(())
}

fn tracing_line(s: &str) { eprintln!("  {s}"); }

/// Reads the entire request body.
///
/// A single `read` is NOT ENOUGH: the save body can be several kilobytes and
/// TCP may split it. Split, the JSON arrives half-formed, the save is rejected
/// with 400 and the user loses their edit — silent data loss.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Result<(String, String)> {
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 { bail!("connection closed"); }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = find(&buf, b"\r\n\r\n") { break p + 4; }
        if buf.len() > 1 << 20 { bail!("headers too large"); }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let want: usize = head.lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:")
            .and_then(|v| v.trim().parse().ok()))
        .unwrap_or(0);
    while buf.len() - head_end < want {
        let n = sock.read(&mut chunk).await?;
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
    Ok((head, body))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

async fn serve(sock: &mut tokio::net::TcpStream, ed: Arc<Editor>) -> Result<()> {
    let (head, body) = read_request(sock).await?;
    let line = head.lines().next().unwrap_or("");

    let (status, ctype, payload): (&str, &str, Vec<u8>) =
        if line.starts_with("GET / ") || line.starts_with("GET /index") {
            ("200 OK", "text/html; charset=utf-8",
             include_str!("../assets/editor.html").as_bytes().to_vec())
        } else if line.starts_with("GET /state.json") {
            let (w, h, v) = *ed.shot_info.lock().await;
            let st = State {
                name: ed.name.clone(), package: ed.package.clone(),
                width: w, height: h, shot: v,
                bindings: ed.bindings.lock().await.clone(),
            };
            ("200 OK", "application/json",
             serde_json::to_vec(&st).unwrap_or_default())
        } else if line.starts_with("GET /shot.png") {
            ("200 OK", "image/png", std::fs::read(&ed.shot).unwrap_or_default())
        } else if line.starts_with("POST /reshot") {
            match screenshot(&ed.shot).await {
                Ok((w, h)) => {
                    let mut g = ed.shot_info.lock().await;
                    *g = (w, h, g.2 + 1);
                    ("200 OK", "application/json",
                     format!("{{\"width\":{w},\"height\":{h},\"shot\":{}}}", g.2).into_bytes())
                }
                Err(e) => ("500 Internal Server Error", "text/plain",
                           e.to_string().into_bytes()),
            }
        } else if line.starts_with("POST /save") {
            match serde_json::from_str::<BTreeMap<String, Binding>>(&body) {
                Ok(m) => match write_back(&ed.path, &m, &ed.name, &ed.package) {
                    Ok(()) => {
                        *ed.bindings.lock().await = m;
                        ("200 OK", "text/plain", b"ok".to_vec())
                    }
                    // The FULL text of the validation error must reach the
                    // browser: saying "invalid" does not tell the user which
                    // key collides, and finding it by hand is very hard.
                    Err(e) => ("400 Bad Request", "text/plain",
                               format!("{e:#}").into_bytes()),
                },
                Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
            }
        } else if line.starts_with("POST /apply") {
            match reload_keymapper().await {
                Ok(m) => ("200 OK", "text/plain", m.as_bytes().to_vec()),
                Err(e) => ("500 Internal Server Error", "text/plain",
                           format!("{e:#}").into_bytes()),
            }
        } else if line.starts_with("POST /poke") {
            match serde_json::from_str::<Poke>(&body) {
                Ok(p) => {
                    // NO delay: the touch now bypasses the compositor and goes
                    // straight to Android, so the game window does not need to
                    // be in front. The user can stay in the browser.
                    tokio::spawn(async move {
                        let _ = crate::keymap::poke(
                            p.x, p.y, p.hold_ms, p.to.map(|t| (t[0], t[1])),
                            liw_core::input::ScreenMap::default(), 0, false).await;
                    });
                    ("200 OK", "text/plain", b"ok".to_vec())
                }
                Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
            }
        } else {
            ("404 Not Found", "text/plain", b"not found".to_vec())
        };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n", payload.len());
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(&payload).await?;
    sock.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liw_core::input::{Norm, Trigger};

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("liw-ed-{}-{name}.toml", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    const BASE: &str = r#"name = "T"
package = "p"

# --- fire button ---
# Bu yorum KORUNMALI.
[bindings.ates]
type = "tap"
trigger = "MouseLeft"
# coordinate measured from the screenshot
at = { x = 0.9, y = 0.2 }
"#;

    /// Comments are half the documentation in profiles; rewriting the file
    /// would delete them and that information is backed up nowhere.
    #[test]
    fn saving_preserves_comments() {
        let p = tmp("comments", BASE);
        let mut b = BTreeMap::new();
        b.insert("ates".into(), Binding::Tap {
            trigger: Trigger::MouseLeft, at: Norm::new(0.5, 0.5) });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("# --- fire button ---"), "{out}");
        assert!(out.contains("# Bu yorum KORUNMALI."), "{out}");
        assert!(out.contains("# coordinate measured from the screenshot"), "{out}");
        assert!(out.contains("0.5"), "the new coordinate must be written: {out}");
    }

    /// Adding a NEW binding must work — the editor's main new capability.
    #[test]
    fn saving_adds_and_removes_bindings() {
        let p = tmp("addremove", BASE);
        let mut b = BTreeMap::new();
        b.insert("zipla".into(), Binding::Tap {
            trigger: Trigger::Key(57), at: Norm::new(0.8, 0.9) });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("[bindings.zipla]"), "eklenmedi: {out}");
        assert!(!out.contains("[bindings.ates]"), "silinmedi: {out}");
        // Tekrar okunabilmeli.
        let re = Profile::from_toml(&out).unwrap();
        assert_eq!(re.bindings.len(), 1);
    }

    /// After a type change the old type's fields must not remain; if they do
    /// the profile stops parsing and the user loses it.
    #[test]
    fn changing_type_drops_stale_fields() {
        let p = tmp("retype", r#"name = "T"
package = "p"
[bindings.hareket]
type = "joystick"
up = { Key = 17 }
down = { Key = 31 }
left = { Key = 30 }
right = { Key = 32 }
center = { x = 0.2, y = 0.7 }
radius = 0.1
"#);
        let mut b = BTreeMap::new();
        b.insert("hareket".into(), Binding::Tap {
            trigger: Trigger::Key(17), at: Norm::new(0.3, 0.4) });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(!out.contains("radius"), "an old field remained: {out}");
        assert!(!out.contains("center"), "an old field remained: {out}");
        Profile::from_toml(&out).expect("must parse again");
    }

    /// Giving the same key to two bindings MUST NOT be saved.
    ///
    /// The engine cannot know which to pick; the user experiences it as
    /// "sometimes it works" and it is very hard to diagnose. We also verify the
    /// file is not touched at all — a half write would be worse.
    #[test]
    fn duplicate_trigger_is_rejected_without_touching_the_file() {
        let p = tmp("dup", BASE);
        let before = std::fs::read_to_string(&p).unwrap();
        let mut b = BTreeMap::new();
        b.insert("a".into(), Binding::Tap {
            trigger: Trigger::Key(17), at: Norm::new(0.1, 0.1) });
        b.insert("b".into(), Binding::Tap {
            trigger: Trigger::Key(17), at: Norm::new(0.2, 0.2) });
        let e = write_back(&p, &b, "T", "p").unwrap_err();
        assert!(format!("{e:#}").contains("duplicate trigger"), "{e:#}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before,
                   "a rejected save must not touch the file");
    }

    /// A TRAILING comment must survive: notes like `# W` are the only place
    /// saying which letter a key code is, and losing them is silent.
    #[test]
    fn saving_preserves_trailing_comments() {
        let p = tmp("trailing", r#"name = "T"
package = "p"
[bindings.hareket]
type = "joystick"
up    = { Key = 17 }             # W
down  = { Key = 31 }             # S
left  = { Key = 30 }             # A
right = { Key = 32 }             # D
center = { x = 0.148, y = 0.738 }
radius = 0.085
"#);
        let mut b = BTreeMap::new();
        b.insert("hareket".into(), Binding::Joystick {
            up: Trigger::Key(17), down: Trigger::Key(31),
            left: Trigger::Key(30), right: Trigger::Key(32),
            center: Norm::new(0.2, 0.7), radius: 0.09 });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        for c in ["# W", "# S", "# A", "# D"] {
            assert!(out.contains(c), "trailing comment '{c}' was lost:\n{out}");
        }
        assert!(out.contains("0.2"), "the new coordinate must be written: {out}");
    }

    /// `f32` noise must not leak into the file.
    ///
    /// Because serialization goes through `f64`, `0.148` was written as
    /// `0.14800000190734863`: the value is right but the file becomes
    /// unreadable and degrades a little on every save.
    #[test]
    fn floats_are_written_in_short_form() {
        let p = tmp("floats", BASE);
        let mut b = BTreeMap::new();
        b.insert("ates".into(), Binding::Tap {
            trigger: Trigger::MouseLeft, at: Norm::new(0.148, 0.738) });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("0.148"), "{out}");
        assert!(out.contains("0.738"), "{out}");
        assert!(!out.contains("0.14800000"), "f32 noise leaked:\n{out}");
        // And it must read back as the SAME f32 — the shortening is lossless.
        let re = Profile::from_toml(&out).unwrap();
        match re.bindings.get("ates").unwrap() {
            Binding::Tap { at, .. } => {
                assert_eq!(at.x, 0.148f32);
                assert_eq!(at.y, 0.738f32);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The PNG size must be read from the header: the UI's scaling maths rests
    /// on the image's REAL size.
    #[test]
    fn png_header_gives_real_size() {
        let mut b = Vec::new();
        b.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&2560u32.to_be_bytes());
        b.extend_from_slice(&1440u32.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        let p = std::env::temp_dir().join(format!("liw-ed-{}.png", std::process::id()));
        std::fs::write(&p, &b).unwrap();
        assert_eq!(png_size(&p), Some((2560, 1440)));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn non_png_is_rejected_not_guessed() {
        let p = tmp("notpng", "this is not a png");
        assert_eq!(png_size(&p), None);
    }
}
