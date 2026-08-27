//! `liw profile edit` — görsel profil düzenleyici.
//!
//! Oyunun ekran görüntüsünü alır, yerel bir HTTP sunucusu açar ve tarayıcıda
//! işaretçileri sürükleyerek koordinat düzenlemeyi sağlar.
//!
//! # Neden web
//!
//! GUI kütüphanesi bağımlılığı eklemeden gerçek görsel düzenleme veriyor.
//! Sunucu yalnızca 127.0.0.1'e bağlanır ve tek profil düzenler.
//!
//! # Doğruluk
//!
//! Düzenleyicinin tek işi koordinat üretmek ve bir kaç pikselik kayma
//! oyunda düğmeyi ıskalamak demek. Bu yüzden:
//!
//! * Tarayıcıya ekran görüntüsünün GERÇEK piksel boyutu bildirilir; arayüz
//!   görüntüyü sığdırmak için ölçekler ama koordinatı her zaman kaynak
//!   piksele geri çevirir.
//! * Yazarken 4 ondalık kullanılır: 2560 pikselde 0.0001 ≈ 0.26 piksel,
//!   yani yuvarlama görünür bir kaymaya yol açamaz. (Eski hâli 3 ondalıktı
//!   ve 2.5 piksele kadar kayabiliyordu.)
//! * Kayıttan önce profil DOĞRULANIR — aynı tuşu iki bağlantıya vermek
//!   "bazen çalışıyor" hatası üretir ve elle fark edilmesi çok zordur.

use anyhow::{bail, Context, Result};
use liw_core::input::{Binding, Profile, Store};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Tarayıcının gördüğü tüm durum.
#[derive(serde::Serialize)]
struct State {
    name: String,
    package: String,
    /// Ekran görüntüsünün gerçek piksel boyutu. Arayüzün ölçekleme
    /// matematiği tamamen buna dayanıyor.
    width: u32,
    height: u32,
    /// Görüntü sürümü: yeniden çekildiğinde artar, tarayıcı önbelleği kırar.
    shot: u64,
    bindings: BTreeMap<String, Binding>,
}

/// Tarayıcıdan gelen tek dokunuş isteği.
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
    /// (genişlik, yükseklik, sürüm)
    shot_info: Mutex<(u32, u32, u64)>,
}

/// Waydroid penceresinin görüntüsünü alır.
///
/// `spectacle -a` (aktif pencere) kullanılıyor. `grim` KWin'de ÇALIŞMIYOR:
/// KWin `wlr-screencopy` protokolünü desteklemiyor.
///
/// `window_geometry()` çağrısı pencereyi tam ekran yapıp AKTİFLEŞTİRİYOR;
/// bu yüzden `-a` tam olarak Waydroid penceresini yakalıyor.
async fn screenshot(out: &Path) -> Result<(u32, u32)> {
    let (_x, _y, w, h) = window_geometry().await?;
    // Pencereyi ÖNE GETİR: spectacle -a aktif pencereyi yakalar ve komut
    // terminalden/tarayıcıdan tetiklendiği için aktif pencere o olur.
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
            // PNG başlığından GERÇEK boyutu oku.
            //
            // KWin'in bildirdiği geometri mantıksal piksel; ekran ölçeği
            // 1 değilse görüntü daha büyük çıkar. Arayüzün ölçekleme
            // matematiği görüntünün kendi boyutuna dayanmalı, yoksa her
            // işaretçi sabit bir oranda kayar.
            if let Some((pw, ph)) = png_size(out) { return Ok((pw, ph)); }
            return Ok((w, h));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    bail!("ekran görüntüsü oluşmadı (spectacle çıkışı: {st})");
}

/// PNG IHDR'den genişlik/yükseklik. Kod çözmeye gerek yok, ilk 24 bayt yeter.
fn png_size(p: &Path) -> Option<(u32, u32)> {
    let b = std::fs::read(p).ok()?;
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    let rd = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    Some((rd(16), rd(20)))
}

/// Keymapper'ı yeniden başlatır (kaydedilen profili yükletmek için).
///
/// Kayıttan sonra profilin etkili olması `liw keymap stop && start` ile
/// yapılıyordu; düzenleyicide çalışıp terminale gitmek düzenleme-dene
/// döngüsünü kırıyor. Burada aynı D-Bus çağrıları yapılıyor.
///
/// Zaten çalışmıyorsa BAŞLATMIYORUZ: kullanıcı bilerek kapatmış olabilir
/// ve düzenleyicinin kendiliğinden cihaz kilitlemesi sürpriz olurdu.
async fn reload_keymapper() -> Result<&'static str> {
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "id.liwinux.Manager1",
        "/id/liwinux/Manager1", "id.liwinux.Manager1").await
        .context("liwd'ye bağlanılamadı")?;
    let st: String = p.call("KeymapperStatus", &()).await
        .context("keymapper durumu okunamadı")?;
    let running = serde_json::from_str::<serde_json::Value>(&st)
        .map(|v| v["running"].as_bool().unwrap_or(false)).unwrap_or(false);
    if !running { return Ok("keymapper zaten kapalı — açmadım"); }

    let grabbed = serde_json::from_str::<serde_json::Value>(&st)
        .map(|v| v["grabbed"].as_bool().unwrap_or(false)).unwrap_or(false);
    p.call::<_, _, ()>("StopKeymapper", &()).await
        .context("keymapper durdurulamadı")?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    p.call::<_, _, ()>("StartKeymapper", &(true,)).await
        .context("keymapper başlatılamadı")?;
    Ok(if grabbed { "keymapper yeniden başlatıldı" }
       else { "keymapper yeniden başlatıldı (kilit oyun kipinde alınır)" })
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

/// Ondalıkları `f32`'nin EN KISA gösterimine indirger.
///
/// Profil koordinatları `f32`; TOML serileştirmesi `f64` üzerinden geçiyor
/// ve `0.148` dosyaya `0.14800000190734863` diye yazılıyordu. Değer doğru
/// ama dosya okunamaz hâle geliyor ve her kayıtta biraz daha büyüyor.
///
/// `f32`'nin `Display`'i zaten gidiş-dönüşü koruyan en kısa ondalığı
/// üretiyor, yani bu indirgeme HİÇBİR hassasiyet kaybetmiyor: aynı `f32`
/// geri okunuyor. 2560 pikselde `f32` çözünürlüğü ~0.0002 piksel.
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

/// Düzenlenen bağlantıları TOML'a geri yazar.
///
/// Dosyayı baştan üretmek YERİNE `toml_edit` ile yerinde güncelleniyor:
/// profillerdeki yorumlar bu projede belgelerin yarısı ve yeniden üretim
/// hepsini silerdi. Yorumlar anahtarın süsünde (decor) tutulduğu için
/// yalnızca DEĞERİ değiştirmek onları korur.
fn write_back(path: &Path, bindings: &BTreeMap<String, Binding>,
              name: &str, package: &str) -> Result<()> {
    // Kayıttan ÖNCE doğrula. En önemlisi aynı tetikleyicinin iki
    // bağlantıda kullanılması: motor hangisini seçeceğini bilemez ve
    // kullanıcı bunu "bazen çalışıyor" diye yaşar.
    Profile { name: name.into(), package: package.into(), bindings: bindings.clone() }
        .validate().context("profil geçersiz")?;

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
        .context("[bindings] bir tablo değil")?;

    // Silinenler.
    for k in tbl.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>() {
        if !bindings.contains_key(&k) { tbl.remove(&k); }
    }

    for (bname, b) in bindings {
        let fresh = toml_edit::ser::to_document(b)
            .with_context(|| format!("'{bname}' TOML'a çevrilemedi"))?;
        let fresh = fresh.as_table();
        match tbl.get_mut(bname).and_then(|i| i.as_table_like_mut()) {
            Some(old) => {
                // Var olan: yalnızca DEĞERLERİ değiştir; anahtar süsünde
                // duran yorumlar yerinde kalsın.
                for (k, v) in fresh.iter() {
                    let mut v = v.clone();
                    normalise_floats(&mut v);
                    match old.get_mut(k) {
                        Some(slot) => {
                            // SATIR SONU yorumunu koru.
                            //
                            // `up = { Key = 17 }   # W` satırındaki "# W",
                            // değerin son-süsünde (suffix decor) duruyor;
                            // değeri düz değiştirmek onu siliyordu. O
                            // yorumlar tuş kodunun hangi harf olduğunu
                            // söyleyen TEK yer — kaybı sessiz ve kalıcı.
                            let keep = slot.as_value().map(|x| x.decor().clone());
                            *slot = v;
                            if let (Some(d), Some(nv)) = (keep, slot.as_value_mut()) {
                                *nv.decor_mut() = d;
                            }
                        }
                        None => { old.insert(k, v); }
                    }
                }
                // Tür değiştiyse eski türe ait anahtarlar kalmamalı.
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
        .with_context(|| format!("'{package}' için profil yok"))?;

    let shot = std::env::temp_dir().join(format!("liw-edit-{}.png", std::process::id()));
    println!("Ekran görüntüsü alınıyor...");
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
                let ed = ed.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(&mut sock, ed).await {
                        tracing_line(&format!("istek hatası: {e}"));
                    }
                });
            }
        }
    }
    let _ = std::fs::remove_file(&ed.shot);
    Ok(())
}

fn tracing_line(s: &str) { eprintln!("  {s}"); }

/// İstek gövdesinin tamamını okur.
///
/// Tek `read` YETMİYOR: kaydetme gövdesi birkaç kilobayt olabiliyor ve TCP
/// onu bölebiliyor. Bölünürse JSON yarım gelir, kayıt "400" ile reddedilir
/// ve kullanıcı düzenlemesini kaybeder — sessiz veri kaybı.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Result<(String, String)> {
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 { bail!("bağlantı kapandı"); }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = find(&buf, b"\r\n\r\n") { break p + 4; }
        if buf.len() > 1 << 20 { bail!("başlık çok büyük"); }
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
                    // Doğrulama hatasının TAM metni tarayıcıya gitmeli:
                    // "geçersiz" demek kullanıcıya hangi tuşun çakıştığını
                    // söylemez ve elle bulması çok zordur.
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
                    // Gecikme YOK: dokunuş artık compositor'ı atlayıp
                    // doğrudan Android'e gidiyor, yani oyun penceresinin
                    // önde olması gerekmiyor. Kullanıcı tarayıcıda kalabilir.
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

# --- ateş düğmesi ---
# Bu yorum KORUNMALI.
[bindings.ates]
type = "tap"
trigger = "MouseLeft"
# koordinat ekran görüntüsünden ölçüldü
at = { x = 0.9, y = 0.2 }
"#;

    /// Yorumlar profillerde belgelerin yarısı; yeniden yazmak onları
    /// silerdi ve o bilgi hiçbir yerde yedeklenmiyor.
    #[test]
    fn saving_preserves_comments() {
        let p = tmp("comments", BASE);
        let mut b = BTreeMap::new();
        b.insert("ates".into(), Binding::Tap {
            trigger: Trigger::MouseLeft, at: Norm::new(0.5, 0.5) });
        write_back(&p, &b, "T", "p").unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("# --- ateş düğmesi ---"), "{out}");
        assert!(out.contains("# Bu yorum KORUNMALI."), "{out}");
        assert!(out.contains("# koordinat ekran görüntüsünden ölçüldü"), "{out}");
        assert!(out.contains("0.5"), "yeni koordinat yazılmalı: {out}");
    }

    /// Yeni bağlantı EKLENEBİLMELİ — düzenleyicinin asıl yeni yeteneği bu.
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

    /// Tür değişince ESKİ türün alanları kalmamalı; kalırsa profil
    /// ayrıştırılamaz hale gelir ve kullanıcı profilini kaybeder.
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
        assert!(!out.contains("radius"), "eski alan kaldı: {out}");
        assert!(!out.contains("center"), "eski alan kaldı: {out}");
        Profile::from_toml(&out).expect("yeniden ayrıştırılabilmeli");
    }

    /// Aynı tuşu iki bağlantıya vermek KAYDEDİLMEMELİ.
    ///
    /// Motor hangisini seçeceğini bilemez; kullanıcı bunu "bazen
    /// çalışıyor" diye yaşar ve teşhisi çok zordur. Dosyaya hiç
    /// dokunulmadığı da doğrulanıyor — yarım yazma daha kötü olurdu.
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
        assert!(format!("{e:#}").contains("aynı tetikleyici"), "{e:#}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before,
                   "reddedilen kayıt dosyaya dokunmamalı");
    }

    /// SATIR SONU yorumu korunmalı: `# W` gibi notlar tuş kodunun hangi
    /// harf olduğunu söyleyen tek yer ve kaybı sessiz.
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
            assert!(out.contains(c), "satır sonu yorumu '{c}' kayboldu:\n{out}");
        }
        assert!(out.contains("0.2"), "yeni koordinat yazılmalı: {out}");
    }

    /// `f32` gürültüsü dosyaya sızmamalı.
    ///
    /// Serileştirme `f64` üzerinden geçtiği için `0.148` dosyaya
    /// `0.14800000190734863` diye yazılıyordu: değer doğru ama dosya
    /// okunamaz hâle geliyor ve her kayıtta biraz daha bozuluyordu.
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
        assert!(!out.contains("0.14800000"), "f32 gürültüsü sızdı:\n{out}");
        // Ve değer AYNI f32 olarak geri okunmalı — kısaltma kayıpsız.
        let re = Profile::from_toml(&out).unwrap();
        match re.bindings.get("ates").unwrap() {
            Binding::Tap { at, .. } => {
                assert_eq!(at.x, 0.148f32);
                assert_eq!(at.y, 0.738f32);
            }
            other => panic!("{other:?}"),
        }
    }

    /// PNG boyutu başlıktan okunmalı: arayüzün ölçekleme matematiği
    /// görüntünün GERÇEK boyutuna dayanıyor.
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
        let p = tmp("notpng", "bu bir png değil");
        assert_eq!(png_size(&p), None);
    }
}
