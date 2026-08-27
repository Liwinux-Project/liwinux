//! liwd — liwinux session daemon.
//!
//! Kullanıcı bağlamında (systemd user servisi) çalışır; session Wayland
//! görüntüsüne eriştiği için root'ta olamaz. Ayrıcalıklı işlemler ileride
//! ayrı bir sistem yardımcısına + polkit'e taşınacak.

mod keymapper;
mod window;

use anyhow::Result;
use liw_core::{Health, SessionState, Supervisor, SupervisorConfig};
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{connection, interface};

/// Pencere yoklaması kaç sağlık turunda bir yapılsın.
///
/// Sağlık turu 5 sn; 6 turda bir = 30 sn. Kullanıcı pencereyi kapatıp
/// açtığında en geç yarım dakikada fark edilir — tam ekran zaten pencere
/// açıldıktan sonra uygulanacak, gecikme hissedilmez.
const WINDOW_POLL_EVERY: u32 = 6;

const BUS_NAME: &str = "id.liwinux.Manager1";
const OBJ_PATH: &str = "/id/liwinux/Manager1";

struct Manager {
    sup: Arc<Supervisor>,
    state: Arc<RwLock<SessionState>>,
    km: Arc<keymapper::Handle>,
    win: Arc<window::WindowState>,
}

#[interface(name = "id.liwinux.Manager1")]
impl Manager {
    /// Session'ı ayrık başlatır. Zaten çalışıyorsa işlemsizdir.
    async fn start(&self) -> zbus::fdo::Result<()> {
        self.sup.start_detached().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn stop(&self) -> zbus::fdo::Result<()> {
        self.sup.stop().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn restart(&self) -> zbus::fdo::Result<()> {
        self.sup.recover().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Süpervizörün en son gözlemlediği durum.
    #[zbus(property)]
    async fn state(&self) -> String {
        self.state.read().await.as_str().to_string()
    }

    /// Ayrıntılı sağlık: JSON. Hangi göstergenin düştüğünü tek tek verir,
    /// çünkü "çalışmıyor" tek başına teşhis için yetersiz.
    async fn health(&self) -> zbus::fdo::Result<String> {
        let h: Health = self.sup.health().await;
        serde_json::to_string(&h).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Keymapper'ı başlatır. Zaten çalışıyorsa işlemsizdir.
    ///
    /// `grab` true ise cihaz YALNIZCA profil etkinken kilitlenir; oyundan
    /// çıkınca bırakılır. Masaüstünde klavyenin çalışmaması kabul edilemez.
    async fn start_keymapper(&self, grab: bool) -> zbus::fdo::Result<()> {
        self.km.start(grab).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    async fn stop_keymapper(&self) -> zbus::fdo::Result<()> {
        self.km.stop().await;
        Ok(())
    }

    /// KWin script'inin pencere geometrisi geri bildirimi.
    async fn report_window_geometry(
        &self, found: bool, x: i32, y: i32, width: i32, height: i32, fullscreen: bool,
    ) -> zbus::fdo::Result<()> {
        self.win.set(window::WindowGeometry {
            found, x, y, width, height, fullscreen,
        }).await;
        if found {
            // Motor en-boy düzeltmesi için gerçek piksel boyutunu ister.
            self.km.set_screen_px(width as u32, height as u32).await;
        }
        Ok(())
    }

    /// Waydroid penceresini tam ekran yapmayı dener.
    async fn fullscreen(&self) -> zbus::fdo::Result<bool> {
        Ok(window::fullscreen_with_retry(
            self.win.clone(), 5, std::time::Duration::from_millis(700)).await)
    }

    /// Waydroid penceresini öne getirir ve odaklar.
    async fn activate_window(&self) -> zbus::fdo::Result<()> {
        window::activate().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Pencere geometrisi (JSON).
    async fn window_geometry(&self) -> zbus::fdo::Result<String> {
        let g = self.win.get().await;
        serde_json::to_string(&g).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// KWin script'inin çağırdığı geri bildirim: hangi pencere odakta.
    ///
    /// Android pencerenin küçültüldüğünü bilmez; bu bilgi olmadan oyun alt
    /// tabdayken bile eşleme sürer ve dokunuşlar masaüstüne düşer.
    async fn set_active_window(&self, class: &str) -> zbus::fdo::Result<()> {
        self.km.set_active_window(class).await;

        // Pencere İLK kez etkinleştiğinde tam ekran yap.
        //
        // Session başlangıcında denemek yetmiyor: pencere o an henüz
        // olmayabilir (`show-full-ui` ayrı bir adım ve kullanıcı ne zaman
        // açacağını biz bilemeyiz). Olay güdümlü olmak tek doğru yol.
        //
        // "İlk kez" şartı bilinçli: kullanıcı tam ekrandan kasten çıktıysa
        // her odaklanmada geri zorlamak düşmanca olur.
        if class.eq_ignore_ascii_case("waydroid")
            && !self.win.fullscreen_attempted().await
            && liw_core::Config::load().fullscreen_on_start
        {
            self.win.mark_fullscreen_attempted().await;
            let w = self.win.clone();
            tokio::spawn(async move {
                window::fullscreen_with_retry(
                    w, 3, std::time::Duration::from_millis(500)).await;
            });
        }
        Ok(())
    }

    /// Keymapper durumu (JSON): çalışıyor mu, ön plan, etkin profil, gecikme.
    async fn keymapper_status(&self) -> zbus::fdo::Result<String> {
        let st = self.km.state().await;
        serde_json::to_string(&st).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(property)]
    async fn keymapper_running(&self) -> bool {
        self.km.state().await.running
    }

    async fn status(&self) -> zbus::fdo::Result<String> {
        let s = self.sup.status().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        serde_json::to_string(&s).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("LIWD_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = SupervisorConfig::default();
    let sup = Arc::new(Supervisor::new(cfg.clone()).with_helper().await);
    let state = Arc::new(RwLock::new(SessionState::Stopped));
    let km = Arc::new(keymapper::Handle::new());
    let win = window::WindowState::new();

    let conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Manager {
            sup: sup.clone(), state: state.clone(),
            km: km.clone(), win: win.clone(),
        })?
        .build()
        .await?;
    tracing::info!("liwd hazır — {BUS_NAME} {OBJ_PATH}");

    // Keymapper'ı kendiliğinden başlat.
    //
    // Önceden yalnızca açık D-Bus çağrısıyla başlıyordu; her
    // `systemctl --user restart liwd` sonrası SESSİZCE kayboluyordu.
    // Kullanıcının gördüğü tek şey "girdileri almıyor" oluyordu.
    //
    // Klavye yapılandırılmamışsa başlatmayı denemek anlamsız — ama bunu
    // sessizce geçmek de aynı hatayı tekrarlamak olurdu, o yüzden söyle.
    {
        let c = liw_core::Config::load();
        if !c.keymapper_on_start {
            tracing::info!("keymapper otomatik başlatma kapalı (keymapper_on_start = false)");
        } else if c.keyboard.is_none() {
            tracing::warn!(
                "keymapper başlatılmadı: yapılandırılmış klavye yok \
                 — `liw keymap detect --save` ile kalibre et");
        } else if let Err(e) = km.start(true).await {
            tracing::error!(hata = %e, "keymapper otomatik başlatılamadı");
        }
    }

    // Adımızı kaybedersek ÇIKMALIYIZ.
    //
    // Gerçekte oldu: eski bir liwd adını kaybetti ama çalışmaya devam etti.
    // Ulaşılamaz bir daemon hâlâ cihaz kilitleyip dokunuş enjekte edebilir —
    // kullanıcının klavyesi çalışmaz ve nedenini bulamaz. systemd zaten
    // yeniden başlatacak; zombi kalmaktansa ölmek doğru.
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let own_id = conn.unique_name().map(|n| n.to_string());

    // --- gözetim döngüsü ---
    let mut unhealthy = 0u32;
    let mut attempts = 0u32;
    let mut was_running = false;
    let mut win_tick: u32 = 0;
    let mut ticker = tokio::time::interval(cfg.poll_interval);
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let h = sup.health().await;
                let next = if !h.session_running {
                    unhealthy = 0; attempts = 0;
                    if was_running { win.reset().await; }
                    was_running = false;
                    SessionState::Stopped
                } else if h.is_healthy() {
                    if unhealthy > 0 { tracing::info!("session toparlandı"); }
                    unhealthy = 0; attempts = 0;

                    // Pencere durumunu SEYREK yokla.
                    //
                    // Sağlık döngüsü 5 saniyede bir dönüyor; her turda KWin
                    // betiği yüklemek saatte 720 yükleme demekti. KWin'in
                    // scripting motorunu bu kadar sık meşgul etmek gereksiz
                    // ve riskli — pencerenin kaybolduğunu fark etmek 5
                    // saniyelik çözünürlük istemiyor.
                    //
                    // Ayrıca YALNIZCA tam ekran denenmişse yoklanır:
                    // denenmediyse sıfırlanacak bir şey de yok.
                    win_tick = win_tick.wrapping_add(1);
                    if was_running
                        && win_tick % WINDOW_POLL_EVERY == 0
                        && win.fullscreen_attempted().await
                    {
                        let _ = window::request_report().await;
                        if win.note_window_gone().await {
                            tracing::info!(
                                "Waydroid penceresi kapandı — tam ekran yeniden denenecek");
                        }
                    }
                    // Session yeni ayağa kalktıysa pencereyi tam ekran yap.
                    // Boot tamamlandığında pencere HENÜZ olmayabilir, o yüzden
                    // tekrar denemeli; ayrıca her döngüde değil YALNIZCA
                    // geçişte tetikliyoruz.
                    if !was_running && liw_core::Config::load().fullscreen_on_start {
                        let w = win.clone();
                        tokio::spawn(async move {
                            window::fullscreen_with_retry(
                                w, 6, std::time::Duration::from_millis(1200)).await;
                        });
                    }
                    was_running = true;
                    SessionState::Running
                } else {
                    unhealthy += 1;
                    tracing::warn!(
                        strike = unhealthy, threshold = cfg.unhealthy_threshold,
                        sorunlar = ?h.failures(), "session sağlıksız");
                    SessionState::Degraded
                };
                *state.write().await = next;

                // Eşik: tek bir dalgalanmada yeniden başlatma yapma.
                if cfg.auto_recover
                    && unhealthy >= cfg.unhealthy_threshold
                    && attempts < cfg.max_recovery_attempts
                {
                    attempts += 1;
                    tracing::error!(deneme = attempts, "kurtarma başlatılıyor");
                    *state.write().await = SessionState::Recovering;
                    if let Err(e) = sup.recover().await {
                        tracing::error!(hata = %e, "kurtarma başarısız");
                    }
                    unhealthy = 0;
                } else if attempts >= cfg.max_recovery_attempts && unhealthy > 0 {
                    tracing::error!(
                        "kurtarma denemeleri tükendi ({}); elle müdahale gerekiyor",
                        cfg.max_recovery_attempts);
                }
            }
            _ = sigterm.recv() => { tracing::info!("SIGTERM — çıkılıyor"); break; }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                // Sahiplik yoklaması. Sinyal dinlemek yerine yoklama:
                // basit, ve 10 saniyelik gecikme zombi riskini kapatmaya yeter.
                match dbus.get_name_owner(BUS_NAME.try_into().unwrap()).await {
                    Ok(owner) if Some(owner.to_string()) == own_id => {}
                    Ok(other) => {
                        tracing::error!(
                            sahip = %other, bizim = ?own_id,
                            "D-Bus adı başkasına geçti — çıkılıyor (zombi kalmamak için)");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(hata = %e, "D-Bus adı sorgulanamadı — çıkılıyor");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT — çıkılıyor"); break; }
        }
    }
    // Keymapper'ı DURDURUYORUZ: kilitli bir cihazı sahipsiz bırakmak
    // kullanıcının klavyesini rehin alır.
    km.stop().await;
    // Not: session'ı kasıtlı olarak durdurmuyoruz. Daemon'un yeniden
    // başlatılması çalışan Android'i öldürmemeli.
    Ok(())
}
