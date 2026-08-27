//! liwd-helper — ayrıcalıklı işlemler için sistem servisi.
//!
//! # Güvenlik tasarımı
//!
//! Bu daemon root olarak çalışır ve sistem veri yolunda dinler. Bu yüzden
//! **genel amaçlı bir kabuk arayüzü AÇMAZ**: `Shell(argv)` gibi bir metot,
//! polkit arkasında bile makinedeki her yerel kullanıcıya root çalıştırma
//! yolu açardı. Onun yerine dar ve adı konmuş işlemler sunulur; her biri
//! kendi polkit eylemine bağlıdır ve girdileri doğrulanır.

mod net;

use anyhow::Result;
use liw_core::{polkit_check, valid_prop_key};
use std::process::Stdio;
use tokio::process::Command;
use zbus::{connection, interface, message::Header, Connection};

const BUS_NAME: &str = "id.liwinux.Helper1";
const OBJ_PATH: &str = "/id/liwinux/Helper1";

const ACT_PROP: &str = "id.liwinux.helper.read-property";
const ACT_DIAG: &str = "id.liwinux.helper.net-diagnose";
const ACT_REPAIR: &str = "id.liwinux.helper.net-repair";
const ACT_OVERLAY: &str = "id.liwinux.helper.debug-overlay";
const ACT_FOREGROUND: &str = "id.liwinux.helper.foreground-app";
const ACT_PERF: &str = "id.liwinux.helper.performance";
const ACT_LOG: &str = "id.liwinux.helper.read-log";
const ACT_AUDIO: &str = "id.liwinux.helper.restart-audio";

/// Ses HAL'inin init'teki tam yolu. Sabit: kullanıcıdan süreç adı almak,
/// istediği şeyi öldürtmek demekti.
const AUDIO_HAL: &str = "/vendor/bin/hw/android.hardware.audio.service";

struct Helper {
    conn: Connection,
}

impl Helper {
    async fn authorize(&self, hdr: &Header<'_>, action: &str, interactive: bool)
        -> zbus::fdo::Result<()>
    {
        let caller = hdr.sender()
            .ok_or_else(|| zbus::fdo::Error::AuthFailed("çağıran kimliği yok".into()))?;
        tracing::debug!(caller = %caller, action, interactive, "polkit sorgusu");
        match polkit_check(&self.conn, caller.as_str(), action, interactive).await {
            Ok(()) => {
                tracing::info!(caller = %caller, action, "yetki verildi");
                Ok(())
            }
            Err(e) => {
                // Ayrımı kaydet: polkit REDDETTİ mi, yoksa polkit'e ULAŞILAMADI mı?
                // İkisi çok farklı sorunlar ve aynı hataya sarılırsa teşhis imkansızlaşır.
                tracing::warn!(caller = %caller, action, hata = %e, "yetkilendirme başarısız");
                Err(zbus::fdo::Error::AccessDenied(format!("{e}")))
            }
        }
    }
}

#[interface(name = "id.liwinux.Helper1")]
impl Helper {
    /// Android property okur. Anahtar karakter kümesi doğrulanır.
    async fn get_prop(
        &self,
        key: &str,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        if !valid_prop_key(key) {
            return Err(zbus::fdo::Error::InvalidArgs(
                format!("geçersiz property anahtarı: {key:?}")));
        }
        self.authorize(&hdr, ACT_PROP, false).await?;
        // "--" ayracı şart: waydroid shell argparse kullanır, tireli
        // argümanları aksi halde yutar.
        let out = Command::new("waydroid")
            .args(["--details-to-stdout", "shell", "--", "getprop", key])
            .stdin(Stdio::null())
            .output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        // Çıkış kodunu kontrol etmemek, başarısızlığı boş string'e çevirir ve
        // çağırana "property boş" diye yalan söyler. Hatayı görünür kıl.
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            tracing::warn!(key, code = ?out.status.code(), stderr = %err,
                           "waydroid shell başarısız");
            return Err(zbus::fdo::Error::Failed(format!(
                "waydroid shell başarısız (kod {:?}): {}", out.status.code(), err)));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string())
    }

    /// Android boot'u tamamladı mı. `liwd` bunu çıkarsamak yerine ölçebilsin diye.
    async fn boot_completed(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<bool> {
        Ok(self.get_prop("sys.boot_completed", hdr).await?.trim() == "1")
    }

    /// Ön plandaki Android uygulamasının paket adını döner.
    ///
    /// Profilin otomatik seçilmesi buna bağlı. `dumpsys activity` çıktısı
    /// Android sürümleri arasında değişebildiği için birden fazla desen
    /// denenir; hiçbiri tutmazsa boş dize döner (uydurma yapmaz).
    async fn foreground_package(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_FOREGROUND, false).await?;
        let out = Command::new("waydroid")
            .args(["--details-to-stdout", "shell", "--",
                   "dumpsys", "activity", "activities"])
            .stdin(Stdio::null())
            .output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(zbus::fdo::Error::Failed(format!("dumpsys başarısız: {err}")));
        }
        Ok(parse_foreground(&String::from_utf8_lossy(&out.stdout)).unwrap_or_default())
    }

    /// SurfaceFlinger katman listesi (ölçüm için).
    async fn surface_layers(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_PERF, false).await?;
        run_dumpsys(&["dumpsys", "SurfaceFlinger", "--list"]).await
    }

    /// Bir katmanın kare zamanlama verisi.
    ///
    /// Katman adı doğrudan komuta gidiyor; kabuk kullanmıyoruz (exec, shell
    /// değil) ama yine de kontrol karakterlerini eliyoruz — argüman
    /// enjeksiyonu bu yolda mümkün olmasa da, girdiyi doğrulamak ucuz.
    async fn surface_latency(
        &self,
        layer: &str,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        if layer.is_empty() || layer.len() > 512
            || layer.chars().any(|c| c.is_control())
        {
            return Err(zbus::fdo::Error::InvalidArgs("geçersiz katman adı".into()));
        }
        self.authorize(&hdr, ACT_PERF, false).await?;
        run_dumpsys(&["dumpsys", "SurfaceFlinger", "--latency", layer]).await
    }

    /// Android'in dokunuş göstergesini (pointer location) açar/kapatır.
    ///
    /// Kalibrasyon için şart: dokunuşun ekranda NEREYE düştüğü görülmeden
    /// koordinat eşlemesi ayarlanamaz. Yalnızca bu geliştirici ayarını
    /// değiştirir; sabit komut, kullanıcıdan gelen dize yok.
    async fn set_pointer_location(
        &self,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.authorize(&hdr, ACT_OVERLAY, false).await?;
        let val = if enabled { "1" } else { "0" };
        for key in ["pointer_location", "show_touches"] {
            let out = Command::new("waydroid")
                .args(["--details-to-stdout", "shell", "--",
                       "settings", "put", "system", key, val])
                .stdin(Stdio::null())
                .output().await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(zbus::fdo::Error::Failed(
                    format!("settings put {key} başarısız: {err}")));
            }
        }
        tracing::info!(enabled, "dokunuş göstergesi ayarlandı");
        Ok(())
    }

    /// Salt okunur ağ teşhisi (JSON). Sistemi değiştirmez, etkileşim istemez.
    /// Android günlüğünün son satırları.
    ///
    /// Uygulama donmalarını teşhis etmenin tek yolu bu. Genel bir `Shell()`
    /// açmamak için argümanlar SIKI kısıtlı: tampon adı sabit listeden
    /// seçilir, satır sayısı sınırlanır. Kullanıcı metni komut satırına
    /// serbestçe geçmiyor.
    async fn logcat(
        &self,
        buffer: &str,
        lines: u32,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let Some(buf) = valid_log_buffer(buffer) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "geçersiz tampon: {buffer:?} (main|crash|system|events|all)")));
        };
        let n = clamp_log_lines(lines);
        self.authorize(&hdr, ACT_LOG, false).await?;
        let n = n.to_string();
        run_dumpsys(&["logcat", "-d", "-b", buf, "-t", &n]).await
    }

    /// Kilitlenmiş ses HAL'ini yeniden başlatır.
    ///
    /// Ölçülen arıza: HAL kilitlenince `audioserver` ona yaptığı
    /// `registerClient` çağrısında takılıyor, gözcü 5 saniye sonra onu
    /// abort ediyor, yeniden başlıyor ve aynı yerde takılıyor. Sonuç:
    /// `AudioFlinger` hiç yayınlanamıyor ve sese dokunan HER uygulama
    /// sonsuza kadar bekliyor — kullanıcıya "oyun açılmıyor" olarak
    /// görünüyor.
    ///
    /// HAL öldürülünce Android'in init'i onu hemen yeniden başlatır.
    /// Tüm oturumu yeniden başlatmaktan çok daha az yıkıcı.
    ///
    /// Süreç adı SABİT: parametre olarak almak, çağırana istediği süreci
    /// root olarak öldürtmek demekti.
    async fn restart_audio(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_AUDIO, true).await?;
        let out = Command::new("pkill").args(["-f", AUDIO_HAL])
            .stdin(Stdio::null()).output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        // pkill: 0 = eşleşti ve sinyal gönderildi, 1 = eşleşme yok.
        // 1'i hata saymak yanlış olurdu ama "yeniden başlatıldı" demek de
        // yalan olur — ayrı raporlanıyor.
        match out.status.code() {
            Some(0) => {
                tracing::info!("ses HAL'i yeniden başlatıldı");
                Ok("ses HAL'i öldürüldü — init yeniden başlatacak".into())
            }
            Some(1) => Ok("ses HAL'i zaten çalışmıyor".into()),
            c => Err(zbus::fdo::Error::Failed(format!("pkill başarısız: {c:?}"))),
        }
    }

    async fn net_diagnose(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_DIAG, false).await?;
        let d = net::diagnose().await;
        serde_json::to_string(&d).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Güvenlik duvarı kurallarını onarır. Yönetici yetkisi ister.
    ///
    /// Yalnızca eksik kural EKLER; mevcut kuralları kaldırmaz ve DNS kaçıran
    /// yabancı tabloları KENDİLİĞİNDEN değiştirmez — başka bir aracın
    /// yapılandırmasını sessizce bozmak kabul edilemez, o durumda rapor eder.
    async fn net_repair(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_REPAIR, true).await?;
        let d = net::diagnose().await;
        let mut done: Vec<String> = Vec::new();

        if d.active_firewall == "ufw" {
            for args in [
                vec!["allow", "in", "on", "waydroid0", "to", "any", "port", "67", "proto", "udp",
                     "comment", "liwinux dhcp"],
                vec!["allow", "in", "on", "waydroid0", "to", "any", "port", "53",
                     "comment", "liwinux dns"],
                vec!["route", "allow", "in", "on", "waydroid0",
                     "comment", "liwinux outbound"],
            ] {
                let st = Command::new("ufw").args(&args).stdin(Stdio::null())
                    .stdout(Stdio::null()).stderr(Stdio::null())
                    .status().await;
                if matches!(st, Ok(s) if s.success()) {
                    // Kuralın tamamını yaz: kırpılmış rapor ("ufw route allow in on")
                    // ne yapıldığını gizler ve denetlenemez hale getirir.
                    done.push(format!("ufw {}", args.join(" ")));
                }
            }
            let _ = Command::new("ufw").arg("reload").stdout(Stdio::null()).status().await;
        }

        if !d.hijack_rules.is_empty() {
            done.push(format!(
                "UYARI: {} yabancı kural DNS'i kaçırıyor; bunlara DOKUNULMADI. \
                 Başka bir aracın yapılandırmasını sessizce değiştirmiyoruz. \
                 Tablolar: {}",
                d.hijack_rules.len(),
                d.hijack_rules.iter().map(|h| h.table.as_str())
                    .collect::<Vec<_>>().join(", ")));
        }
        if done.is_empty() { done.push("yapılacak bir şey bulunamadı".into()); }
        Ok(done.join("\n"))
    }
}

/// `waydroid shell -- <argv>` çalıştırır ve stdout'u temizleyip döner.
///
/// "--" ayracı ŞART: waydroid shell argparse kullanır, tireli argümanları
/// aksi halde yutar.
/// Logcat tampon adını sabit listeye karşı doğrular.
///
/// Serbest metin kabul etmek, argümanın komut satırına geçmesi demekti.
/// Sabit listeden DÖNDÜRÜLEN dize kullanılır; girdinin kendisi asla
/// komuta gitmez.
fn valid_log_buffer(b: &str) -> Option<&'static str> {
    match b {
        "main" => Some("main"),
        "crash" => Some("crash"),
        "system" => Some("system"),
        "events" => Some("events"),
        "all" => Some("all"),
        _ => None,
    }
}

/// Satır sayısını makul aralığa sıkıştırır.
///
/// Üst sınır şart: sınırsız logcat D-Bus mesaj boyutunu aşar ve çağrı
/// sessizce başarısız olur.
fn clamp_log_lines(n: u32) -> u32 { n.clamp(1, 2000) }

async fn run_dumpsys(argv: &[&str]) -> zbus::fdo::Result<String> {
    let mut args = vec!["--details-to-stdout", "shell", "--"];
    args.extend_from_slice(argv);
    let out = Command::new("waydroid").args(&args).stdin(Stdio::null())
        .output().await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(zbus::fdo::Error::Failed(format!(
            "waydroid shell başarısız (kod {:?}): {}", out.status.code(), err)));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `dumpsys activity activities` çıktısından ön plan paketini çıkarır.
///
/// Ayrı fonksiyon ki gerçek çıktılara karşı test edilebilsin — Android
/// sürümleri bu çıktının biçimini değiştiriyor ve sessizce yanlış paket
/// döndürmek yanlış profili yükler.
fn parse_foreground(dump: &str) -> Option<String> {
    // Sırayla dene: en güvenilir desen önce.
    for line in dump.lines() {
        let t = line.trim();
        for key in ["mResumedActivity:", "topResumedActivity=", "mFocusedActivity:"] {
            if let Some(rest) = t.split_once(key).map(|(_, r)| r) {
                if let Some(pkg) = extract_pkg(rest) { return Some(pkg); }
            }
        }
    }
    None
}

/// "... u0 com.kiloo.subwaysurf/com.sybogames...Activity t42}" içinden
/// paket adını ayıklar.
fn extract_pkg(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|tok| tok.contains('/') && tok.contains('.'))
        .and_then(|tok| tok.split('/').next())
        .map(|p| p.trim_start_matches('{').to_string())
        .filter(|p| p.contains('.') && !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{clamp_log_lines, valid_log_buffer};
    #[test]
    fn log_buffer_allowlist_rejects_injection() {
        for bad in ["main; rm -rf /", "--help", "", "MAIN", "main main",
                    "-b", "../etc/passwd"] {
            assert!(valid_log_buffer(bad).is_none(), "{bad:?} reddedilmeliydi");
        }
        for good in ["main", "crash", "system", "events", "all"] {
            assert_eq!(valid_log_buffer(good), Some(good));
        }
    }

    /// Sınırsız logcat D-Bus mesaj sınırını aşar ve çağrı sessizce ölür.
    #[test]
    fn log_lines_are_bounded() {
        assert_eq!(clamp_log_lines(0), 1);
        assert_eq!(clamp_log_lines(500), 500);
        assert_eq!(clamp_log_lines(u32::MAX), 2000);
    }

    use super::{extract_pkg, parse_foreground};

    const REAL: &str = "  mResumedActivity: ActivityRecord{ef108a1 u0 com.kiloo.subwaysurf/com.sybogames.chili.multidex.ChiliMultidexSupportActivity t42}";

    #[test]
    fn parses_resumed_activity() {
        assert_eq!(parse_foreground(REAL).as_deref(), Some("com.kiloo.subwaysurf"));
    }

    #[test]
    fn parses_top_resumed_variant() {
        let s = "topResumedActivity=ActivityRecord{abc u0 com.android.vending/.AssistActivity t9}";
        assert_eq!(parse_foreground(s).as_deref(), Some("com.android.vending"));
    }

    /// Tanınmayan biçimde paket UYDURMAMALI.
    #[test]
    fn unknown_format_yields_none() {
        assert!(parse_foreground("alakasız çıktı\nbaşka satır").is_none());
    }

    #[test]
    fn ignores_tokens_without_package_shape() {
        assert!(extract_pkg(" u0 t42}").is_none());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("LIWD_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let conn = Connection::system().await?;
    let _srv = connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Helper { conn })?
        .build()
        .await?;
    tracing::info!("liwd-helper hazır — {BUS_NAME} (root, polkit korumalı)");

    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT"),
    }
    Ok(())
}
