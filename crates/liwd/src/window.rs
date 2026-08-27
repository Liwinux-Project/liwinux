//! Waydroid penceresinin KWin üzerinden yönetimi.
//!
//! Neden gerekli: dokunuşlar EKRAN uzayında gidiyor. Pencere çıkışla hizalı
//! değilse profil koordinatları kayar ve kenar dokunuşları pencerenin dışına
//! düşer. Ölçüldü: pencere 10,10 / 2540x1370 iken x=0.0 ve x=1.0 kayboluyordu.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

const SCRIPT_NAME: &str = "liwinux-fullscreen";
const ACTIVATE_SCRIPT: &str = "liwinux-activate";
const REPORT_SCRIPT: &str = "liwinux-report";

/// KWin script'inin bildirdiği pencere geometrisi.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct WindowGeometry {
    pub found: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub fullscreen: bool,
}

#[derive(Default)]
pub struct WindowState {
    geometry: RwLock<WindowGeometry>,
    /// Bu session'da tam ekran bir kez denendi mi. Session düşünce sıfırlanır.
    attempted: RwLock<bool>,
}

impl WindowState {
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }

    pub async fn set(&self, g: WindowGeometry) {
        *self.geometry.write().await = g;
    }

    pub async fn get(&self) -> WindowGeometry {
        *self.geometry.read().await
    }

    pub async fn fullscreen_attempted(&self) -> bool { *self.attempted.read().await }
    pub async fn mark_fullscreen_attempted(&self) { *self.attempted.write().await = true; }

    /// Session düştüğünde çağrılır: yeni session'da yeniden denensin.
    pub async fn reset(&self) {
        *self.attempted.write().await = false;
        *self.geometry.write().await = WindowGeometry::default();
    }

    /// Pencere kaybolduysa tam ekran bayrağını sıfırlar.
    ///
    /// Session'ın durmasını beklemek YETMİYORDU: kullanıcı session'ı ayakta
    /// tutup `show-full-ui` penceresini kapatıp açıyor. O durumda bayrak
    /// sıfırlanmadığı için yeni pencere hiçbir zaman tam ekran yapılmıyordu
    /// — 10 saatlik bir daemon ömrü boyunca tek deneme. Gerçekte yaşandı.
    ///
    /// Pencere DURUYOR ama tam ekran değilse dokunulmuyor: kullanıcı kasten
    /// çıkmış olabilir ve her seferinde geri zorlamak düşmanca olur.
    ///
    /// `true` dönerse yeni pencere için deneme yapılabilir.
    pub async fn note_window_gone(&self) -> bool {
        // found == false → pencere YOK. Duruyorsa hiçbir şey yapılmaz.
        if self.geometry.read().await.found { return false; }
        std::mem::replace(&mut *self.attempted.write().await, false)
    }
}

/// Tam ekran script'ini bir kez çalıştırır.
///
/// Sonuç `ReportWindowGeometry` ile geri gelir; bu fonksiyon yalnızca
/// tetikler. KWin scripting API'si çıktıyı çağırana döndürmez.
pub async fn request_fullscreen() -> Result<()> {
    let path = script_path().context("fullscreen.js bulunamadı")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    // Aynı ad iki kez yüklenirse iki kez çalışır; önce temizle.
    let _: Result<bool, _> = p.call("unloadScript", &(SCRIPT_NAME,)).await;
    let _id: i32 = p.call("loadScript", &(path.to_string_lossy().as_ref(), SCRIPT_NAME))
        .await.context("loadScript başarısız")?;
    let _: () = p.call("start", &()).await.context("script başlatılamadı")?;
    Ok(())
}

/// Waydroid penceresini öne getirir ve odaklar.
///
/// Ekran görüntüsü almadan önce şart: `spectacle -a` AKTİF pencereyi
/// yakalar. Aktif pencere terminal olursa terminalin görüntüsü alınır.
pub async fn activate() -> Result<()> {
    let path = script_path_named("activate.js").context("activate.js bulunamadı")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: Result<bool, _> = p.call("unloadScript", &(ACTIVATE_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript",
        &(path.to_string_lossy().as_ref(), ACTIVATE_SCRIPT)).await?;
    let _: () = p.call("start", &()).await?;
    Ok(())
}

/// Pencere durumunu sorar — HİÇBİR ŞEYİ DEĞİŞTİRMEZ.
///
/// Sonuç `ReportWindowGeometry` ile geri gelir. Durumu öğrenmek için
/// `request_fullscreen` çağırmak, sırf bakmak isterken kullanıcının kasten
/// çıktığı tam ekranı geri zorlamak olurdu.
pub async fn request_report() -> Result<()> {
    let path = script_path_named("report.js").context("report.js bulunamadı")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: Result<bool, _> = p.call("unloadScript", &(REPORT_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript",
        &(path.to_string_lossy().as_ref(), REPORT_SCRIPT)).await?;
    let _: () = p.call("start", &()).await?;
    Ok(())
}

/// Pencere görünene kadar birkaç kez dener.
///
/// Boot tamamlandığında pencere HENÜZ OLMAYABİLİR: `show-full-ui` ayrı bir
/// adım ve compositor yüzeyi oluşturması zaman alır. Tek deneme çoğu zaman
/// erken gelir.
pub async fn fullscreen_with_retry(
    state: Arc<WindowState>,
    attempts: u32,
    gap: std::time::Duration,
) -> bool {
    for i in 1..=attempts {
        if let Err(e) = request_fullscreen().await {
            tracing::warn!(deneme = i, hata = %e, "tam ekran isteği gönderilemedi");
        }
        tokio::time::sleep(gap).await;
        let g = state.get().await;
        if g.found && g.fullscreen {
            tracing::info!(
                genişlik = g.width, yükseklik = g.height,
                "Waydroid penceresi tam ekran");
            return true;
        }
    }
    let g = state.get().await;
    if g.found {
        tracing::warn!(
            genişlik = g.width, yükseklik = g.height, x = g.x, y = g.y,
            "pencere bulundu ama tam ekran yapılamadı — dokunuş koordinatları kayabilir");
    } else {
        tracing::info!("Waydroid penceresi yok — tam ekran atlandı");
    }
    false
}

/// `fullscreen.js` arar. Çalışma dizinine BAKILMAZ (profil deposundaki
/// aynı ders: dizine bağlı davranış teşhis edilemeyen hatalar üretir).
fn script_path() -> Option<std::path::PathBuf> { script_path_named("fullscreen.js") }

fn script_path_named(name: &str) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".local").join("share")))
    {
        candidates.push(data.join("liwinux").join("kwin").join(name));
    }
    candidates.push(format!("/usr/share/liwinux/kwin/{name}").into());
    candidates.push(format!("/usr/local/share/liwinux/kwin/{name}").into());
    for c in candidates {
        if c.is_file() { return Some(c); }
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    for _ in 0..4 {
        let cand = dir.join("scripts").join("kwin").join(name);
        if cand.is_file() { return Some(cand); }
        dir = dir.parent()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Session yeniden başlayınca tam ekran YENİDEN denenmeli.
    #[tokio::test]
    async fn reset_clears_attempt_flag() {
        let s = WindowState::new();
        s.mark_fullscreen_attempted().await;
        assert!(s.fullscreen_attempted().await);
        s.reset().await;
        assert!(!s.fullscreen_attempted().await);
        assert!(!s.get().await.found);
    }

    #[tokio::test]
    async fn geometry_starts_empty_and_updates() {
        let s = WindowState::new();
        assert!(!s.get().await.found);
        s.set(WindowGeometry {
            found: true, x: 0, y: 0, width: 2560, height: 1440, fullscreen: true,
        }).await;
        let g = s.get().await;
        assert!(g.found && g.fullscreen);
        assert_eq!((g.width, g.height), (2560, 1440));
    }

    /// Pencere kapanınca tam ekran yeniden denenebilmeli.
    ///
    /// Gerçekte yaşandı: kullanıcı session'ı ayakta tutup `show-full-ui`
    /// penceresini kapatıp açtı; bayrak sıfırlanmadığı için 10 saat boyunca
    /// hiçbir yeni pencere tam ekran yapılmadı.
    #[tokio::test]
    async fn closed_window_reenables_fullscreen_attempt() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, x: 0, y: 0,
            width: 2560, height: 1440, fullscreen: true }).await;
        s.mark_fullscreen_attempted().await;

        // Pencere duruyorken dokunulmamalı.
        assert!(!s.note_window_gone().await, "pencere dururken sıfırlanmamalı");
        assert!(s.fullscreen_attempted().await);

        // Pencere kayboldu.
        s.set(WindowGeometry::default()).await;
        assert!(s.note_window_gone().await, "kapanma bildirilmeli");
        assert!(!s.fullscreen_attempted().await, "bayrak sıfırlanmalı");
    }

    /// Kullanıcı tam ekrandan KASTEN çıktıysa geri zorlanmamalı.
    #[tokio::test]
    async fn user_leaving_fullscreen_is_not_fought() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, x: 100, y: 100,
            width: 1280, height: 720, fullscreen: false }).await;
        s.mark_fullscreen_attempted().await;
        assert!(!s.note_window_gone().await);
        assert!(s.fullscreen_attempted().await, "kullanıcının kararı korunmalı");
    }

    /// Pencere hiç olmadıysa ve deneme yapılmadıysa bildirilecek bir şey yok.
    #[tokio::test]
    async fn nothing_to_report_when_never_attempted() {
        let s = WindowState::new();
        assert!(!s.note_window_gone().await);
    }

    /// Aynı kapanma iki kez bildirilmemeli — yoksa her yoklamada
    /// tam ekran zorlanır ve kullanıcıyla kavga edilir.
    #[tokio::test]
    async fn closure_is_reported_once() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, ..Default::default() }).await;
        s.mark_fullscreen_attempted().await;
        s.set(WindowGeometry::default()).await;
        assert!(s.note_window_gone().await);
        assert!(!s.note_window_gone().await, "ikinci kez bildirilmemeli");
    }
}
