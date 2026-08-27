//! Keymapper çalışma döngüsü.
//!
//! `liw keymap run` ve `liwd` aynı motoru kullansın diye buraya taşındı.
//!
//! # Bağımsızlık
//!
//! Runner ön plandaki uygulamayı **kendisi sorgulamaz**; bir kanaldan alır.
//! Böylece keymapper Waydroid'i, D-Bus'ı veya polkit'i bilmez — yalnızca
//! "şu an şu paket ön planda" bilgisini tüketir. Ön planı kimin nasıl
//! bulduğu çağıranın sorunudur.

use super::backend::TouchBackend;
use super::capture::{translate, GrabbedDevice};
use super::engine::{Engine, InputEvent, TriggerKind};
use super::latency::LatencyStats;
use super::store::Store;
use super::uinput::{ScreenMap, UinputBackend};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// evdev tuş kodu: ESC.
const KEY_ESC: u16 = 1;
/// Kilitliyken çıkış için kaç kez ESC gerekiyor. Oyunda ESC'ye basmakla
/// kazara çıkmayı önler.
const ESC_STREAK: u8 = 3;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("cihaz açılamadı: {0}")]
    Device(#[from] super::capture::CaptureError),
    #[error("dokunmatik arka uç kurulamadı: {0}")]
    Backend(#[from] super::backend::BackendError),
    #[error("olay akışı koptu: {0}")]
    Stream(#[source] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Dinlenecek klavye.
    pub device: PathBuf,
    /// Dinlenecek fare. Yoksa fare eşlemesi (Aim, fare tuşları) çalışmaz.
    pub mouse: Option<PathBuf>,
    /// Kilitleme özelliği açık mı.
    ///
    /// Açıkken kilit OTOMATİK alınmaz: kullanıcı `hotkey` ile oyun kipine
    /// geçer. Profil etkinleşir etkinleşmez kilitlemek menülerde sıkışmaya
    /// yol açıyordu.
    pub grab: bool,
    /// Oyun kipini açıp kapatan evdev tuş kodu.
    pub hotkey: Option<u16>,
    /// Dokunmatik koordinat eşlemesi.
    pub screen_map: ScreenMap,
    /// Hedef pencerenin piksel boyutu.
    ///
    /// Joystick dairesinin gerçekten daire olması buna bağlı: normalize
    /// koordinat eksen başına ölçekli olduğu için 2560x1440'ta aynı
    /// normalize yarıçap yatayda 1.78 kat uzağa gider.
    pub screen_px: (u32, u32),
}

/// Dışarıdan gözlemlenebilir durum.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunnerState {
    pub running: bool,
    /// Ön plandaki paket (profil olsun olmasın).
    pub foreground: Option<String>,
    /// Etkin profilin adı; profil yoksa `None`.
    pub active_profile: Option<String>,
    pub grabbed: bool,
    /// Oyun kipi açık mı (kilit + eşleme). Kapalıyken fare serbest.
    pub game_mode: bool,
    /// Waydroid penceresi host'ta odakta mı. Değilse eşleme durur.
    pub host_focused: bool,
    /// Bizim katmanımızın gecikmesi (mikrosaniye, p50/p99).
    pub latency_p50_us: u64,
    pub latency_p99_us: u64,
}

/// Döngüden dışarı bildirilen olaylar (loglama/arayüz için).
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    ProfileActivated { package: String, profile: String },
    ProfileCleared { package: String },
    Grabbed,
    Ungrabbed,
    GameModeOn,
    GameModeOff,
    FocusGained,
    FocusLost,
    EscapeRequested,
    /// Sistem katmanı (asistan, bildirim paneli) oyunun üstüne çıktı.
    ///
    /// `ProfileCleared`'dan ayrı tutuluyor çünkü nedeni tamamen farklı:
    /// oyun kapanmadı, üstüne bir şey geldi. Kullanıcı farenin neden
    /// öldüğünü bilmeli.
    OverlayPaused { package: String },
}

/// Oyunun ÜSTÜNE çıkan geçici sistem katmanları.
///
/// Bunlar ön plana geçtiğinde oyun kapanmış olmuyor, sadece üstü
/// örtülüyor. Profili silmek yerine duraklatıyoruz ki katman kalkınca
/// anında geri dönsün.
///
/// Liste dar tutuluyor: izin diyalogları gibi gerçekten kullanıcı
/// girdisi bekleyen şeyler BURAYA GİRMEMELİ.
pub fn is_system_overlay(pkg: &str) -> bool {
    matches!(pkg,
        // Google Asistan / arama — oyun sırasında kendiliğinden açılıyor.
        "com.google.android.googlequicksearchbox"
        // Bildirim paneli, son kullanılanlar, ses kontrolü.
        | "com.android.systemui")
}

pub struct Runner {
    cfg: RunnerConfig,
    store: Store,
    state: Arc<RwLock<RunnerState>>,
}

impl Runner {
    pub fn new(cfg: RunnerConfig, store: Store) -> Self {
        Self { cfg, store, state: Arc::new(RwLock::new(RunnerState::default())) }
    }

    pub fn state(&self) -> Arc<RwLock<RunnerState>> { self.state.clone() }
    pub fn store(&self) -> &Store { &self.store }

    /// Döngüyü çalıştırır. `foreground` kanalı kapanınca veya `shutdown`
    /// tetiklenince temiz çıkar.
    ///
    /// Çıkışta parmaklar HER ZAMAN kaldırılır ve kilit bırakılır — süreç
    /// çökse bile çekirdek fd kapanışında kilidi bırakır, ama temiz çıkışta
    /// beklemeye gerek yok.
    /// `host_focused`: Waydroid penceresi host'ta odakta mı.
    ///
    /// Bu kapı ŞART: Android pencerenin küçültüldüğünü bilmez, oyun alt
    /// tabdayken bile kendini ön planda sanar. Kapı olmadan dokunuşlar
    /// kullanıcının gerçek masaüstüne düşer.
    pub async fn run(
        &mut self,
        mut foreground: mpsc::Receiver<String>,
        mut host_focused: tokio::sync::watch::Receiver<bool>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        events: Option<mpsc::Sender<RunnerEvent>>,
    ) -> Result<LatencyStats, RunnerError> {
        let mut backend = UinputBackend::new(self.cfg.screen_map)?;
        // libinput/KWin'in cihazı tanıması birkaç yüz ms sürebilir.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let dev = GrabbedDevice::open(&self.cfg.device, false)?;
        let mut stream = dev.into_stream()?;

        // Fare AYRI bir cihaz. Klavyeyle aynı akışta gelmez; ikisini de
        // dinlemek zorundayız yoksa Aim ve fare tuşları hiç tetiklenmez.
        let mut mouse_stream = match &self.cfg.mouse {
            Some(p) => match GrabbedDevice::open(p, false) {
                Ok(d) => match d.into_stream() {
                    Ok(s) => Some(s),
                    Err(e) => { tracing::warn!(hata = %e, "fare akışı kurulamadı"); None }
                },
                Err(e) => { tracing::warn!(hata = %e, "fare açılamadı"); None }
            },
            None => None,
        };

        let mut engine: Option<Engine> = None;
        // Etkin profil, motordan AYRI tutulur: odak kaybolunca motoru
        // söküyoruz ama profili unutmuyoruz ki odak dönünce geri kuralım.
        let mut profile: Option<super::profile::Profile> = None;
        let mut focused = *host_focused.borrow();
        // Kısayol tanımlıysa oyun kipi KAPALI başlar; kullanıcı hazır
        // olduğunda açar. Tanımlı değilse eski davranış: profil varsa açık.
        let mut game_mode = self.cfg.hotkey.is_none();
        let mut current: Option<String> = None;
        let mut grabbed = false;
        let mut esc = 0u8;
        let mut lat = LatencyStats::new();

        let t0 = std::time::Instant::now();
        // Kare başına tek dokunuş hareketi göndermek için: gerçek
        // dokunmatik ekranlar 60-240 Hz raporlar, 1000 Hz fare hızında
        // değil. 5 ms ~ 200 Hz.
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        {
            let mut s = self.state.write().await;
            s.running = true;
        }
        let emit = |e: RunnerEvent| {
            if let Some(tx) = &events { let _ = tx.try_send(e); }
        };

        loop {
            tokio::select! {
                // --- ön plan değişimi ---
                Some(pkg) = foreground.recv() => {
                    if current.as_deref() == Some(pkg.as_str()) { continue; }

                    // Sistem katmanı: oyun kapanmadı, üstü örtüldü.
                    // Profili KORU ki katman kalkınca anında dönsün.
                    // Kilidi bırak ki kullanıcı katmanı fareyle kapatabilsin
                    // — yoksa fare kilitli kalır ve asistan kapatılamaz.
                    if is_system_overlay(&pkg) && profile.is_some() {
                        if let Some(e) = engine.as_mut() {
                            let acts = e.set_enabled(false);
                            let _ = backend.dispatch(&acts);
                        }
                        let _ = backend.release_all();
                        engine = None;
                        if grabbed {
                            let _ = stream.device_mut().ungrab();
                            if let Some(m) = mouse_stream.as_mut() {
                                let _ = m.device_mut().ungrab();
                            }
                            grabbed = false;
                            emit(RunnerEvent::Ungrabbed);
                        }
                        emit(RunnerEvent::OverlayPaused { package: pkg.clone() });
                        let mut s = self.state.write().await;
                        s.foreground = Some(pkg);
                        s.grabbed = false;
                        continue;
                    }

                    // Eski profili kapat ve parmakları bırak: uygulama
                    // değişirken ekranda asılı parmak kalmamalı.
                    if let Some(e) = engine.as_mut() {
                        let acts = e.set_enabled(false);
                        let _ = backend.dispatch(&acts);
                    }
                    let _ = backend.release_all();

                    match self.store.for_package(&pkg) {
                        Some(entry) => {
                            profile = Some(entry.profile.clone());
                            emit(RunnerEvent::ProfileActivated {
                                package: pkg.clone(),
                                profile: entry.profile.name.clone(),
                            });
                            // Motor yalnızca host odaktayken kurulur.
                            if focused && game_mode {
                                engine = Some({
                                    let mut e = Engine::new(entry.profile.clone());
                                    e.set_aspect(self.cfg.screen_px.0, self.cfg.screen_px.1);
                                    e
                                });
                                if self.cfg.grab && !grabbed
                                    && stream.device_mut().grab().is_ok()
                                {
                                    // Fare de kilitlenmeli: yoksa imleç host'ta
                                    // gezinir ve tıklamalar masaüstüne gider.
                                    if let Some(m) = mouse_stream.as_mut() {
                                        let _ = m.device_mut().grab();
                                    }
                                    grabbed = true;
                                    emit(RunnerEvent::Grabbed);
                                }
                            }
                            let mut s = self.state.write().await;
                            s.active_profile = Some(entry.profile.name.clone());
                        }
                        None => {
                            profile = None;
                            engine = None;
                            emit(RunnerEvent::ProfileCleared { package: pkg.clone() });
                            if grabbed {
                                let _ = stream.device_mut().ungrab();
                                if let Some(m) = mouse_stream.as_mut() {
                                    let _ = m.device_mut().ungrab();
                                }
                                grabbed = false;
                                emit(RunnerEvent::Ungrabbed);
                            }
                            let mut s = self.state.write().await;
                            s.active_profile = None;
                        }
                    }
                    current = Some(pkg.clone());
                    let mut s = self.state.write().await;
                    s.foreground = Some(pkg);
                    s.grabbed = grabbed;
                }

                // --- host odak değişimi ---
                _ = host_focused.changed() => {
                    let now_focused = *host_focused.borrow();
                    if now_focused == focused { continue; }
                    focused = now_focused;
                    if focused {
                        if let Some(p) = &profile.clone().filter(|_| game_mode) {
                            engine = Some({
                                let mut e = Engine::new(p.clone());
                                e.set_aspect(self.cfg.screen_px.0, self.cfg.screen_px.1);
                                e
                            });
                            if self.cfg.grab && !grabbed
                                && stream.device_mut().grab().is_ok()
                            {
                                if let Some(m) = mouse_stream.as_mut() {
                                    let _ = m.device_mut().grab();
                                }
                                grabbed = true;
                                emit(RunnerEvent::Grabbed);
                            }
                        }
                        emit(RunnerEvent::FocusGained);
                    } else {
                        // Odak kaybında parmakları BIRAK ve kilidi çöz:
                        // aksi halde tuşlar kullanıcının masaüstüne dokunuş
                        // enjekte etmeye devam eder.
                        if let Some(e) = engine.as_mut() {
                            let acts = e.set_enabled(false);
                            let _ = backend.dispatch(&acts);
                        }
                        engine = None;
                        let _ = backend.release_all();
                        if grabbed {
                            let _ = stream.device_mut().ungrab();
                            if let Some(m) = mouse_stream.as_mut() {
                                let _ = m.device_mut().ungrab();
                            }
                            grabbed = false;
                            emit(RunnerEvent::Ungrabbed);
                        }
                        emit(RunnerEvent::FocusLost);
                    }
                    let mut s = self.state.write().await;
                    s.host_focused = focused;
                    s.grabbed = grabbed;
                }

                // --- jest saati ---
                _ = ticker.tick(), if engine.as_ref().is_some_and(|e| e.has_pending()) => {
                    if let Some(e) = engine.as_mut() {
                        let acts = e.tick(t0.elapsed().as_millis() as u64);
                        if !acts.is_empty() { let _ = backend.dispatch(&acts); }
                    }
                }

                // --- girdi ---
                ev = stream.next_event() => {
                    let ev = ev.map_err(RunnerError::Stream)?;
                    let ev_time = ev.timestamp();
                    let Some(input) = translate(&ev) else { continue };

                    // Kısayol HER ZAMAN dinlenir — kilitli olsun olmasın.
                    // Kilitliyken dinlenmezse oyun kipinden çıkılamaz.
                    if let (Some(hk), InputEvent::Press(TriggerKind::Key(k))) =
                        (self.cfg.hotkey, input)
                    {
                        if k == hk {
                            game_mode = !game_mode;
                            if game_mode {
                                if let Some(p) = &profile {
                                    if focused {
                                        engine = Some({
                                let mut e = Engine::new(p.clone());
                                e.set_aspect(self.cfg.screen_px.0, self.cfg.screen_px.1);
                                e
                            });
                                        if self.cfg.grab && !grabbed
                                            && stream.device_mut().grab().is_ok()
                                        {
                                            if let Some(m) = mouse_stream.as_mut() {
                                                let _ = m.device_mut().grab();
                                            }
                                            grabbed = true;
                                            emit(RunnerEvent::Grabbed);
                                        }
                                    }
                                }
                                emit(RunnerEvent::GameModeOn);
                            } else {
                                // Kipten çıkarken parmakları BIRAK: yoksa
                                // menüye dönüldüğünde ekranda asılı kalır.
                                if let Some(e) = engine.as_mut() {
                                    let acts = e.set_enabled(false);
                                    let _ = backend.dispatch(&acts);
                                }
                                engine = None;
                                let _ = backend.release_all();
                                if grabbed {
                                    let _ = stream.device_mut().ungrab();
                                    if let Some(m) = mouse_stream.as_mut() {
                                        let _ = m.device_mut().ungrab();
                                    }
                                    grabbed = false;
                                    emit(RunnerEvent::Ungrabbed);
                                }
                                emit(RunnerEvent::GameModeOff);
                            }
                            let mut s = self.state.write().await;
                            s.game_mode = game_mode;
                            s.grabbed = grabbed;
                            continue;
                        }
                    }

                    if grabbed {
                        match input {
                            InputEvent::Press(TriggerKind::Key(KEY_ESC)) => {
                                esc += 1;
                                if esc >= ESC_STREAK {
                                    emit(RunnerEvent::EscapeRequested);
                                    break;
                                }
                                continue;
                            }
                            InputEvent::Press(_) => esc = 0,
                            _ => {}
                        }
                    }

                    if let Some(e) = engine.as_mut() {
                        // tick eylemleri ATILMAMALI: önceki jestin UP'ı orada olabilir.
                        let mut acts = e.tick(t0.elapsed().as_millis() as u64);
                        acts.extend(e.handle(input));
                        if !acts.is_empty() && backend.dispatch(&acts).is_ok() {
                            lat.record(ev_time);
                        }
                    }
                }

                // --- fare girdisi ---
                ev = async {
                    match mouse_stream.as_mut() {
                        Some(m) => m.next_event().await,
                        // Fare yoksa bu kol asla hazır olmamalı.
                        None => std::future::pending().await,
                    }
                } => {
                    let ev = ev.map_err(RunnerError::Stream)?;
                    let ev_time = ev.timestamp();
                    if let (Some(input), Some(e)) = (translate(&ev), engine.as_mut()) {
                        // Fare hareketi motorda BİRİKİR; uygulama tick'te.
                        // Tuş olayları (fare düğmeleri) anında işlenir.
                        let acts = e.handle(input);
                        if !acts.is_empty() && backend.dispatch(&acts).is_ok() {
                            lat.record(ev_time);
                        }
                    }
                }

                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }

        // Temiz çıkış.
        if let Some(e) = engine.as_mut() {
            let acts = e.set_enabled(false);
            let _ = backend.dispatch(&acts);
        }
        let _ = backend.release_all();
        if grabbed {
            let _ = stream.device_mut().ungrab();
            if let Some(m) = mouse_stream.as_mut() { let _ = m.device_mut().ungrab(); }
        }

        let (p50, _, p99, _) = lat.percentiles();
        let mut s = self.state.write().await;
        s.running = false;
        s.grabbed = false;
        s.active_profile = None;
        s.latency_p50_us = p50;
        s.latency_p99_us = p99;
        Ok(lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_idle() {
        let s = RunnerState::default();
        assert!(!s.running);
        assert!(s.active_profile.is_none());
        assert!(!s.grabbed);
        assert!(!s.game_mode);
        // Odak varsayılanı KAPALI: bilinmezken açık saymak masaüstüne
        // dokunuş enjekte etme riski demek.
        assert!(!s.host_focused);
    }

    /// Kaçış eşiği 1 olmamalı: oyunda ESC'ye basmak kazara çıkmaya yol açmasın.
    #[test]
    fn escape_requires_repeated_presses() {
        assert!(ESC_STREAK > 1);
    }

    /// Asistan oyunun üstüne çıkınca profil DÜŞMEMELİ.
    ///
    /// Gerçekte yaşandı: asistan kendiliğinden açıldı, ön plan paketi
    /// değişti, keymapper kapandı ve fare sessizce öldü.
    #[test]
    fn assistant_and_systemui_are_overlays() {
        assert!(is_system_overlay("com.google.android.googlequicksearchbox"));
        assert!(is_system_overlay("com.android.systemui"));
    }

    /// Gerçek uygulamalar katman sayılmamalı — yoksa oyundan çıkınca
    /// eşleme açık kalır ve tuşlar masaüstüne sızar.
    #[test]
    fn real_apps_are_not_overlays() {
        for p in ["com.ForgeGames.SpecialForcesGroup2", "com.android.settings",
                  "com.android.permissioncontroller", "com.android.vending"] {
            assert!(!is_system_overlay(p), "{p} katman sayılmamalı");
        }
    }

    #[test]
    fn state_is_serialisable_for_dbus() {
        let s = RunnerState { running: true, foreground: Some("com.x".into()),
            active_profile: Some("P".into()), grabbed: true, host_focused: true,
            game_mode: true, latency_p50_us: 80, latency_p99_us: 170 };
        let j = serde_json::to_string(&s).unwrap();
        let back: RunnerState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.active_profile.as_deref(), Some("P"));
        assert_eq!(back.latency_p99_us, 170);
    }
}
