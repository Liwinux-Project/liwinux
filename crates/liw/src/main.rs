//! liw — liwinux komut satırı istemcisi.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use liw_core::{Health, Supervisor, SupervisorConfig};
use zbus::Connection;

const BUS_NAME: &str = "id.liwinux.Manager1";
const OBJ_PATH: &str = "/id/liwinux/Manager1";

#[derive(Parser)]
#[command(name = "liw", version, about = "liwinux — Linux'ta Android oyun platformu")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Session yönetimi
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Session'ı başlat (terminalden bağımsız)
    Start,
    /// Session'ı durdur
    Stop,
    /// Session'ı yeniden başlat
    Restart,
    /// Durum özeti
    Status,
    /// Ayrıntılı sağlık kontrolü — hangi göstergenin düştüğünü söyler
    Health,
}

/// Daemon varsa ona konuş; yoksa doğrudan Waydroid'e düş.
///
/// Doğrudan mod bilinçli bir taviz: daemon kurulu olmayan bir sistemde de
/// `liw` kullanışlı olmalı. Ancak bu modda otomatik kurtarma YOKTUR.
async fn manager() -> Option<zbus::Proxy<'static>> {
    let conn = Connection::session().await.ok()?;
    let p = zbus::Proxy::new(&conn, BUS_NAME, OBJ_PATH, BUS_NAME).await.ok()?;
    p.introspect().await.ok()?;
    Some(p)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let Cmd::Session { action } = cli.cmd;
    let proxy = manager().await;
    if proxy.is_none() {
        eprintln!("uyarı: liwd çalışmıyor — doğrudan kipte, otomatik kurtarma yok");
    }
    let sup = Supervisor::new(SupervisorConfig::default()).with_helper().await;

    match action {
        SessionAction::Start => {
            match &proxy {
                Some(p) => p.call::<_, _, ()>("Start", &()).await.context("Start çağrısı")?,
                None => sup.start_detached().await.context("session başlatma")?,
            }
            println!("session başlatıldı");
        }
        SessionAction::Stop => {
            match &proxy {
                Some(p) => p.call::<_, _, ()>("Stop", &()).await.context("Stop çağrısı")?,
                None => sup.stop().await.context("session durdurma")?,
            }
            println!("session durduruldu");
        }
        SessionAction::Restart => {
            match &proxy {
                Some(p) => p.call::<_, _, ()>("Restart", &()).await.context("Restart çağrısı")?,
                None => sup.recover().await.context("yeniden başlatma")?,
            }
            println!("session yeniden başlatıldı");
        }
        SessionAction::Status => {
            let s = sup.status().await.context("durum okunamadı")?;
            println!("Session   : {}", s.session);
            println!("Container : {}", s.container);
            println!("IP        : {}", s.ip.as_deref().unwrap_or("-"));
            if let Some(p) = &proxy {
                if let Ok(st) = p.get_property::<String>("State").await {
                    println!("liwd      : {st}");
                }
            } else {
                println!("liwd      : çalışmıyor");
            }
        }
        SessionAction::Health => {
            let h: Health = match &proxy {
                Some(p) => {
                    let json: String = p.call("Health", &()).await.context("Health çağrısı")?;
                    serde_json::from_str(&json).context("sağlık verisi çözümlenemedi")?
                }
                None => sup.health().await,
            };
            let mark = |b: bool| if b { "OK  " } else { "HATA" };
            println!("  {} session çalışıyor", mark(h.session_running));
            println!("  {} konteyner çalışıyor", mark(h.container_running));
            println!("  {} composer HAL canlı", mark(h.composer_alive));
            println!("  {} Android boot tamamlandı", mark(h.boot_completed));
            println!("  {} IP atanmış", mark(h.has_ip));
            println!();
            if h.is_healthy() {
                println!("session sağlıklı");
            } else {
                println!("SORUNLAR:");
                for f in h.failures() { println!("  - {f}"); }
                if !h.composer_alive {
                    println!();
                    println!("composer ölümü çökme zincirinin köküdür:");
                    println!("  composer -> SurfaceFlinger SIGABRT -> system_server -> tüm uygulamalar");
                    println!("Kurtarmak için: liw session restart");
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
