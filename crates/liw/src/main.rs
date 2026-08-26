//! liw — liwinux komut satırı istemcisi.

mod keymap;

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
    /// Tuş eşleme
    Keymap {
        #[command(subcommand)]
        action: KeymapAction,
    },
}

#[derive(Subcommand)]
enum KeymapAction {
    /// Kullanılabilir girdi cihazlarını listele
    Devices,
    /// Android dokunuş göstergesini aç/kapat (kalibrasyon görsel yardımı)
    Overlay {
        /// kapat
        #[arg(long)]
        off: bool,
    },
    /// Koordinat taraması: hangi noktaların pencereye ulaştığını ölç
    Sweep {
        /// Eksen: x veya y
        #[arg(default_value = "x")]
        axis: char,
        /// Nokta sayısı
        #[arg(long, default_value_t = 11)]
        count: u32,
        /// Noktalar arası bekleme (ms)
        #[arg(long, default_value_t = 900)]
        gap: u64,
    },
    /// Klavyeyi kalibrasyonla belirle: bir tuşa bas
    Detect {
        /// Bulunan cihazı yapılandırmaya kaydet
        #[arg(short, long)]
        save: bool,
    },
    /// Hangi cihazın hangi tuş kodunu ürettiğini izle (teşhis)
    Watch {
        /// Tek bir cihazı izle (varsayılan: tüm klavyeler)
        #[arg(short, long)]
        device: Option<std::path::PathBuf>,
    },
    /// Tek dokunuş/sürükleme gönder — eşlemeden bağımsız enjeksiyon testi
    Poke {
        /// X (0..1)
        #[arg(default_value_t = 0.5)]
        x: f32,
        /// Y (0..1)
        #[arg(default_value_t = 0.5)]
        y: f32,
        /// Basılı tutma / sürükleme süresi (ms)
        #[arg(long, default_value_t = 120)]
        hold: u64,
        /// Sürükleme hedefi: --to X,Y
        #[arg(long)]
        to: Option<String>,
        /// Dokunmatik uzayda hedef bölge: ORIGIN_X,ORIGIN_Y,SCALE_X,SCALE_Y
        #[arg(long)]
        region: Option<String>,
        /// X eksenini aynala
        #[arg(long)]
        invert_x: bool,
        /// Y eksenini aynala
        #[arg(long)]
        invert_y: bool,
    },
    /// Profili gerçek klavyeyle dene (Android'e enjeksiyon YOK)
    Test {
        /// Profil dosyası (.toml)
        profile: std::path::PathBuf,
        /// Belirli bir cihaz kullan (varsayılan: ilk klavye)
        #[arg(short, long)]
        device: Option<std::path::PathBuf>,
        /// Cihazı kilitle — tuşlar masaüstüne gitmez
        #[arg(short, long)]
        grab: bool,
        /// Ekran genişliği (piksel dönüşümü için)
        #[arg(long, default_value_t = 1920)]
        width: u32,
        /// Ekran yüksekliği
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Dokunuşları GERÇEKTEN enjekte et (sanal dokunmatik ekran)
        #[arg(short, long)]
        inject: bool,
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
    let action = match cli.cmd {
        Cmd::Keymap { action } => {
            return match action {
                KeymapAction::Devices => keymap::list_devices(),
                KeymapAction::Overlay { off } => keymap::overlay(!off).await,
                KeymapAction::Sweep { axis, count, gap } => keymap::sweep(axis, count, gap).await,
                KeymapAction::Detect { save } => keymap::detect(save).await,
                KeymapAction::Watch { device } => keymap::watch(device).await,
                KeymapAction::Poke { x, y, hold, to, region, invert_x, invert_y } => {
                    let drag = match to {
                        Some(s) => {
                            let (a, b) = s.split_once(',')
                                .context("--to biçimi: X,Y  (örn: 0.2,0.5)")?;
                            Some((a.trim().parse()?, b.trim().parse()?))
                        }
                        None => None,
                    };
                    let mut map = liw_core::input::ScreenMap::default();
                    if let Some(r) = region {
                        let v: Vec<f32> = r.split(',')
                            .map(|p| p.trim().parse::<f32>())
                            .collect::<Result<_, _>>()
                            .context("--region biçimi: OX,OY,SX,SY")?;
                        anyhow::ensure!(v.len() == 4, "--region dört sayı ister: OX,OY,SX,SY");
                        map.origin_x = v[0]; map.origin_y = v[1];
                        map.scale_x = v[2];  map.scale_y = v[3];
                    }
                    map.invert_x = invert_x;
                    map.invert_y = invert_y;
                    keymap::poke(x, y, hold, drag, map).await
                }
                KeymapAction::Test { profile, device, grab, width, height, inject } =>
                    keymap::test_profile(profile, device, grab, (width, height), inject).await,
            };
        }
        Cmd::Session { action } => action,
    };
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
