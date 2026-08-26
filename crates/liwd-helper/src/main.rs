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

    /// Salt okunur ağ teşhisi (JSON). Sistemi değiştirmez, etkileşim istemez.
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
