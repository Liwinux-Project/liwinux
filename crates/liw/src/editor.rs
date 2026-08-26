//! `liw profile edit` — görsel profil düzenleyici.
//!
//! Ekran görüntüsü alır, yerel bir HTTP sunucusu açar ve tarayıcıda
//! sürüklenebilir işaretçilerle koordinat düzenlemeyi sağlar.
//!
//! # Neden web
//!
//! GUI kütüphanesi bağımlılığı eklemeden gerçek görsel düzenleme veriyor.
//! Bu GEÇİCİ bir araç; kalıcı çözüm Faz 5'teki arayüz.
//!
//! Sunucu yalnızca 127.0.0.1'e bağlanır ve tek profil düzenler.

use anyhow::{bail, Context, Result};
use liw_core::input::{Binding, Store};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Düzenleyiciye gönderilen/alınan bağlantı.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Mark {
    name: String,
    kind: String,
    x: f64,
    y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    radius: Option<f64>,
    /// TOML'da hangi alan güncellenecek: at | center | origin | from
    field: String,
}

fn marks_of(p: &liw_core::input::Profile) -> Vec<Mark> {
    p.bindings.iter().map(|(name, b)| {
        let (kind, x, y, radius, field) = match b {
            Binding::Tap { at, .. } => ("tap", at.x, at.y, None, "at"),
            Binding::Toggle { at, .. } => ("toggle", at.x, at.y, None, "at"),
            Binding::Aim { origin, .. } => ("aim", origin.x, origin.y, None, "origin"),
            Binding::Joystick { center, radius, .. } =>
                ("joystick", center.x, center.y, Some(*radius as f64), "center"),
            Binding::Swipe { from, .. } => ("swipe", from.x, from.y, None, "from"),
        };
        Mark { name: name.clone(), kind: kind.into(),
               x: x as f64, y: y as f64, radius, field: field.into() }
    }).collect()
}

/// Waydroid penceresinin görüntüsünü alır.
///
/// `spectacle -a` (aktif pencere) kullanılıyor. `grim` KWin'de ÇALIŞMIYOR:
/// KWin `wlr-screencopy` protokolünü desteklemiyor ve grim
/// "compositor doesn't support the screen capture protocol" diyor.
///
/// `window_geometry()` çağrısı pencereyi tam ekran yapıp AKTİFLEŞTİRİYOR;
/// bu yüzden `-a` tam olarak Waydroid penceresini yakalıyor.
async fn screenshot(out: &PathBuf) -> Result<(u32, u32)> {
    let (_x, _y, w, h) = window_geometry().await?;
    // Pencereyi ÖNE GETİR: spectacle -a aktif pencereyi yakalar ve komut
    // terminalden çalıştırıldığı için aktif pencere terminaldir.
    activate_window().await?;
    // KWin'in odağı devretmesi ve pencerenin çizilmesi için pay.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let _ = std::fs::remove_file(out);
    let st = tokio::process::Command::new("spectacle")
        .args(["-a", "-b", "-n", "-o", out.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status().await.context("spectacle çalıştırılamadı (kurulu mu?)")?;
    // spectacle arka planda yazar; dosyanın belirmesini bekle.
    for _ in 0..30 {
        if out.is_file() && std::fs::metadata(out).map(|m| m.len() > 0).unwrap_or(false) {
            return Ok((w, h));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    bail!("ekran görüntüsü oluşmadı (spectacle çıkışı: {st})");
}

async fn activate_window() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await?;
    p.call::<_, _, ()>("ActivateWindow", &()).await
        .context("pencere öne getirilemedi")?;
    Ok(())
}

async fn window_geometry() -> Result<(i32, i32, u32, u32)> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await
        .context("liwd'ye bağlanılamadı")?;
    // Önce tam ekran isteği: geometri güncellensin.
    let _: Result<bool, _> = p.call("Fullscreen", &()).await;
    let json: String = p.call("WindowGeometry", &()).await
        .context("pencere geometrisi alınamadı")?;
    let v: serde_json::Value = serde_json::from_str(&json)?;
    if !v["found"].as_bool().unwrap_or(false) {
        bail!("Waydroid penceresi bulunamadı — oyun açık mı?");
    }
    Ok((v["x"].as_i64().unwrap_or(0) as i32,
        v["y"].as_i64().unwrap_or(0) as i32,
        v["width"].as_u64().unwrap_or(0) as u32,
        v["height"].as_u64().unwrap_or(0) as u32))
}

fn write_back(path: &PathBuf, marks: &[Mark]) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    for m in marks {
        let Some(b) = doc.get_mut("bindings").and_then(|t| t.get_mut(&m.name))
        else { continue };
        let Some(target) = b.get_mut(&m.field) else { continue };
        let mut t = toml_edit::InlineTable::new();
        // 3 basamak yeter: 2560 pikselde 0.001 ≈ 2.5 piksel.
        let r = |v: f64| (v * 1000.0).round() / 1000.0;
        t.insert("x", toml_edit::value(r(m.x)).into_value().unwrap());
        t.insert("y", toml_edit::value(r(m.y)).into_value().unwrap());
        *target = toml_edit::Item::Value(toml_edit::Value::InlineTable(t));
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

pub async fn run(package: &str, port: u16) -> Result<()> {
    let store = Store::discover();
    let entry = store.for_package(package)
        .with_context(|| format!("'{package}' için profil yok"))?;
    let path = entry.path.clone();
    let profile = entry.profile.clone();

    let shot = std::env::temp_dir().join(format!("liw-edit-{}.png", std::process::id()));
    println!("Ekran görüntüsü alınıyor...");
    let (w, h) = screenshot(&shot).await?;
    println!("  {w}x{h} -> {}", shot.display());

    let marks = std::sync::Arc::new(tokio::sync::Mutex::new(marks_of(&profile)));
    let html = include_str!("../assets/editor.html")
        .replace("<script>\nconst S =",
            &format!("<script id=\"data\" type=\"application/json\">{}</script>\n<script>\nconst S =",
                serde_json::json!({
                    "name": profile.name, "package": profile.package,
                    "bindings": *marks.lock().await,
                })));

    let listener = TcpListener::bind(("127.0.0.1", port)).await
        .with_context(|| format!("port {port} açılamadı"))?;
    let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    println!();
    println!("Düzenleyici: {url}");
    println!("Tarayıcı açılıyor — bitince bu terminalde Ctrl+C ile çık.");
    let _ = tokio::process::Command::new("xdg-open").arg(&url)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().await;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { println!(); println!("kapatıldı"); break; }
            acc = listener.accept() => {
                let (mut sock, _) = acc?;
                let (html, shot, path, marks) =
                    (html.clone(), shot.clone(), path.clone(), marks.clone());
                tokio::spawn(async move {
                    let _ = serve(&mut sock, &html, &shot, &path, marks).await;
                });
            }
        }
    }
    let _ = std::fs::remove_file(&shot);
    Ok(())
}

async fn serve(
    sock: &mut tokio::net::TcpStream, html: &str, shot: &PathBuf,
    path: &PathBuf, marks: std::sync::Arc<tokio::sync::Mutex<Vec<Mark>>>,
) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let line = req.lines().next().unwrap_or("");
    let body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");

    let (status, ctype, payload): (&str, &str, Vec<u8>) =
        if line.starts_with("GET / ") {
            ("200 OK", "text/html; charset=utf-8", html.as_bytes().to_vec())
        } else if line.starts_with("GET /shot.png") {
            ("200 OK", "image/png", std::fs::read(shot).unwrap_or_default())
        } else if line.starts_with("POST /save") {
            match serde_json::from_str::<Vec<Mark>>(body)
                .map_err(anyhow::Error::from)
                .and_then(|m| { let r = write_back(path, &m); *futures_lock(&marks) = m; r })
            {
                Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
                Err(e) => ("500 Internal Server Error", "text/plain",
                           e.to_string().into_bytes()),
            }
        } else if line.starts_with("POST /poke") {
            let name = line.split("b=").nth(1)
                .and_then(|s| s.split_whitespace().next())
                .map(|s| percent_decode(s)).unwrap_or_default();
            let m = futures_lock(&marks).iter().find(|m| m.name == name).cloned();
            match m {
                Some(m) => {
                    tokio::spawn(async move {
                        let _ = crate::keymap::poke(
                            m.x as f32, m.y as f32, 250, None,
                            liw_core::input::ScreenMap::default(), 3).await;
                    });
                    ("200 OK", "text/plain", b"ok".to_vec())
                }
                None => ("404 Not Found", "text/plain", b"yok".to_vec()),
            }
        } else {
            ("404 Not Found", "text/plain", b"yok".to_vec())
        };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n", payload.len());
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(&payload).await?;
    sock.flush().await?;
    Ok(())
}

/// Basit blocking kilit: bu sunucu tek kullanıcılıdır, çekişme yok.
fn futures_lock<T>(m: &std::sync::Arc<tokio::sync::Mutex<T>>)
    -> tokio::sync::MutexGuard<'_, T>
{
    m.blocking_lock()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v); i += 3; continue;
            }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
