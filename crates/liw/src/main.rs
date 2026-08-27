//! liw — liwinux komut satırı istemcisi.

mod bench;
mod perf;
mod trace;
mod editor;
mod keymap;
mod profile;

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
    /// Performans ölçümü: kare zamanlaması ve kaynak kullanımı
    Bench {
        /// Android paket adı
        package: String,
        /// Ölçüm süresi (saniye)
        #[arg(short, long, default_value_t = 60)]
        duration: u64,
    },
    /// Takılmanın NEDENİNİ bul: kare + Android günlüğü + host, aynı saatte
    Trace {
        /// Android paket adı
        package: String,
        /// İzleme süresi (saniye)
        #[arg(short, long, default_value_t = 90)]
        duration: u64,
        /// Takılma eşiği (ms). Verilmezse yenileme hızından türetilir.
        #[arg(long)]
        jank_ms: Option<f64>,
    },
    /// Performans kaldıraçlarının teşhisi (hiçbir şey değiştirmez)
    Perf {
        #[command(subcommand)]
        action: PerfAction,
    },
    /// Profil yönetimi
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Tuş eşleme
    Keymap {
        #[command(subcommand)]
        action: KeymapAction,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// Bulunan tüm profilleri listele
    List,
    /// Bir profilin ayrıntısını göster
    Show {
        /// Android paket adı
        package: String,
    },
    /// Ön plandaki uygulama için hangi profil geçerli
    Which,
    /// GÖRSEL düzenleyiciyi aç (ekran görüntüsü üstünde sürükle-bırak)
    Edit {
        /// Android paket adı
        package: String,
        /// Sunucu portu (0 = rastgele)
        #[arg(long, default_value_t = 8731)]
        port: u16,
    },
    /// Bir bağlantının koordinatını değiştir
    Set {
        /// Android paket adı
        package: String,
        /// Bağlantı adı (liw profile show ile bak)
        binding: String,
        /// X (0..1)
        x: f64,
        /// Y (0..1)
        y: f64,
        /// Değiştirilecek alan: at | center | origin | from | to
        #[arg(long, default_value = "at")]
        field: String,
    },
    /// Bir bağlantının koordinatına dokun — yerleşimi görsel doğrula
    Poke {
        package: String,
        binding: String,
        /// Dokunmadan önce bekle (hedef pencereyi öne getirmek için)
        #[arg(long, default_value_t = 5)]
        delay: u64,
    },
    /// Depoyla gelen profilleri kullanıcı dizinine kopyala
    Install {
        /// Var olanların üzerine yaz
        #[arg(short, long)]
        force: bool,
        /// Kaynak profil dizini (kurulu binary'de gerekli)
        #[arg(long)]
        from: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum KeymapAction {
    /// Kullanılabilir girdi cihazlarını listele
    Devices,
    /// Keymapper'ı liwd içinde başlat (terminal kapansa da sürer)
    Start {
        /// Profil etkinken cihazı kilitle
        #[arg(short, long)]
        grab: bool,
    },
    /// liwd içindeki keymapper'ı durdur
    Stop,
    /// liwd içindeki keymapper'ın durumu
    Status,
    /// Keymapper'ı ÖN PLANDA çalıştır (hata ayıklama; Ctrl+C ile biter)
    Run {
        /// Profil etkinken cihazı kilitle (tuşlar masaüstüne gitmez)
        #[arg(short, long)]
        grab: bool,
        /// Ön plan yoklama aralığı (ms)
        #[arg(long, default_value_t = 1000)]
        poll: u64,
    },
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
        /// Klavye yerine FAREYİ kalibre et (hareket ettirerek)
        #[arg(short, long)]
        mouse: bool,
        /// Oyun kipi KISAYOL tuşunu belirle
        #[arg(long)]
        hotkey: bool,
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
        /// Dokunmadan önce bekle (saniye) — hedef pencereyi öne getirmek için
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Eski uinput yolunu zorla (varsayılan: Waydroid dokunuş borusu)
        #[arg(long)]
        uinput: bool,
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
enum PerfAction {
    /// Kaldıraçların şu anki halini oku ve raporla
    Status,
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
    /// Waydroid penceresini tam ekran yap
    Fullscreen,
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
        Cmd::Bench { package, duration } => return bench::run(package, duration).await,
        Cmd::Trace { package, duration, jank_ms } =>
            return trace::run(package, duration, jank_ms).await,
        Cmd::Perf { action } => return match action {
            PerfAction::Status => perf::status(),
        },
        Cmd::Profile { action } => {
            return match action {
                ProfileAction::List => profile::list(),
                ProfileAction::Show { package } => profile::show(&package),
                ProfileAction::Which => profile::which().await,
                ProfileAction::Edit { package, port } => editor::run(&package, port).await,
                ProfileAction::Set { package, binding, x, y, field } =>
                    profile::set_coord(&package, &binding, &field, x, y),
                ProfileAction::Poke { package, binding, delay } =>
                    profile::poke_binding(&package, &binding, delay).await,
                ProfileAction::Install { force, from } => profile::install(force, from),
            };
        }
        Cmd::Keymap { action } => {
            return match action {
                KeymapAction::Devices => keymap::list_devices(),
                KeymapAction::Start { grab } => keymap::daemon_start(grab).await,
                KeymapAction::Stop => keymap::daemon_stop().await,
                KeymapAction::Status => keymap::daemon_status().await,
                KeymapAction::Run { grab, poll } => keymap::run(grab, poll).await,
                KeymapAction::Overlay { off } => keymap::overlay(!off).await,
                KeymapAction::Sweep { axis, count, gap } => keymap::sweep(axis, count, gap).await,
                KeymapAction::Detect { save, mouse, hotkey } =>
                    keymap::detect(save, mouse, hotkey).await,
                KeymapAction::Watch { device } => keymap::watch(device).await,
                KeymapAction::Poke { x, y, hold, to, region, invert_x, invert_y, delay, uinput } => {
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
                    keymap::poke(x, y, hold, drag, map, delay, uinput).await
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
        SessionAction::Fullscreen => {
            let p = proxy.as_ref()
                .context("liwd çalışmıyor — systemctl --user status liwd")?;
            let ok: bool = p.call("Fullscreen", &()).await.context("Fullscreen çağrısı")?;
            let json: String = p.call("WindowGeometry", &()).await.unwrap_or_default();
            if ok {
                println!("pencere tam ekran");
            } else {
                println!("tam ekran yapılamadı");
            }
            if !json.is_empty() { println!("geometri: {json}"); }
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
            println!("  {} composer bağlantısı taze", mark(!h.composer_stale));
            println!("  {} Android boot tamamlandı", mark(h.boot_completed));
            println!("  {} IP atanmış", mark(h.has_ip));
            println!();
            if h.is_healthy() {
                println!("session sağlıklı");
            } else {
                println!("SORUNLAR:");
                for f in h.failures() { println!("  - {f}"); }
                if h.composer_stale {
                    println!();
                    println!("composer session'dan sonra yeniden başlamış. Süreçler ayakta");
                    println!("görünür ama binder bağlantısı bayattır: pencere açılmaz,");
                    println!("'waydroid app launch' 'Sending reply failed' döner.");
                    println!("Kurtarmak için: liw session restart");
                }
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
