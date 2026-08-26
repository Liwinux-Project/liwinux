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
    /// Profil etkinken cihazı kilitle.
    pub grab: bool,
    /// Dokunmatik koordinat eşlemesi.
    pub screen_map: ScreenMap,
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
    EscapeRequested,
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
    pub async fn run(
        &mut self,
        mut foreground: mpsc::Receiver<String>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        events: Option<mpsc::Sender<RunnerEvent>>,
    ) -> Result<LatencyStats, RunnerError> {
        let mut backend = UinputBackend::new(self.cfg.screen_map)?;
        // libinput/KWin'in cihazı tanıması birkaç yüz ms sürebilir.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        let dev = GrabbedDevice::open(&self.cfg.device, false)?;
        let mut stream = dev.into_stream()?;

        let mut engine: Option<Engine> = None;
        let mut current: Option<String> = None;
        let mut grabbed = false;
        let mut esc = 0u8;
        let mut lat = LatencyStats::new();

        let t0 = std::time::Instant::now();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(4));
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

                    // Eski profili kapat ve parmakları bırak: uygulama
                    // değişirken ekranda asılı parmak kalmamalı.
                    if let Some(e) = engine.as_mut() {
                        let acts = e.set_enabled(false);
                        let _ = backend.dispatch(&acts);
                    }
                    let _ = backend.release_all();

                    match self.store.for_package(&pkg) {
                        Some(entry) => {
                            engine = Some(Engine::new(entry.profile.clone()));
                            emit(RunnerEvent::ProfileActivated {
                                package: pkg.clone(),
                                profile: entry.profile.name.clone(),
                            });
                            if self.cfg.grab && !grabbed
                                && stream.device_mut().grab().is_ok()
                            {
                                grabbed = true;
                                emit(RunnerEvent::Grabbed);
                            }
                            let mut s = self.state.write().await;
                            s.active_profile = Some(entry.profile.name.clone());
                        }
                        None => {
                            engine = None;
                            emit(RunnerEvent::ProfileCleared { package: pkg.clone() });
                            if grabbed {
                                let _ = stream.device_mut().ungrab();
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
        if grabbed { let _ = stream.device_mut().ungrab(); }

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
    }

    /// Kaçış eşiği 1 olmamalı: oyunda ESC'ye basmak kazara çıkmaya yol açmasın.
    #[test]
    fn escape_requires_repeated_presses() {
        assert!(ESC_STREAK > 1);
    }

    #[test]
    fn state_is_serialisable_for_dbus() {
        let s = RunnerState { running: true, foreground: Some("com.x".into()),
            active_profile: Some("P".into()), grabbed: true,
            latency_p50_us: 80, latency_p99_us: 170 };
        let j = serde_json::to_string(&s).unwrap();
        let back: RunnerState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.active_profile.as_deref(), Some("P"));
        assert_eq!(back.latency_p99_us, 170);
    }
}
