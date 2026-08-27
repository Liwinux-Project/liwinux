//! Waydroid'in dokunuş borusuna DOĞRUDAN yazan arka uç.
//!
//! # Neden bu yol
//!
//! Waydroid, Android'in `EventHub`'ını yamalar ve konteyner içinde üç
//! isimlendirilmiş boru dinler
//! (`anbox-patches/frameworks/native/0006-EventHub-Add-wayland-inputs-support.patch`):
//!
//! ```text
//! /dev/input/wl_touch_events     → EventHub cihazı "wayland_touch"
//! /dev/input/wl_pointer_events   → "wayland_pointer"
//! /dev/input/wl_keyboard_events  → "wayland_keyboard"
//! ```
//!
//! Normalde bu boruya **hwcomposer** yazar: compositor'dan `wl_touch` alır,
//! `input_event` kayıtlarına çevirir. Yani bizim uinput arka ucumuzun uzun
//! yolu (uinput → libinput → KWin → wl_touch → hwcomposer → boru) sonunda
//! zaten buraya varıyordu. Doğrudan yazmak dört halkayı birden atar.
//!
//! # Asıl kazanç: kırpma yok
//!
//! Boru yolunda koordinatı ekrana sıkıştıran hiçbir katman yok:
//!
//! * **Çekirdek yok.** Boru bir FIFO'dur; evdev sürücü katmanı devrede
//!   değil, `ABS` aralık kırpması çalışmaz.
//! * **`TouchInputMapper::cookPointerData()` kırpmaz.** Yalnızca afin
//!   dönüşüm + ölçekleme uygular; yüzey sınırı testi yoktur.
//! * **`InputDispatcher` MOVE'da pencere aramaz.** Hedef yalnızca
//!   `ACTION_DOWN`/`ACTION_POINTER_DOWN` anında seçilir; sonrası
//!   mandallanmış duruma gider.
//!
//! Dolayısıyla oyun penceresi içinde inen bir parmak, sonraki hareketleri
//! ekranın dışına taşsa bile aynı pencereye ulaşmaya devam eder. FPS
//! nişanında "kenara gelince kaldır ve ortala" zorunluluğu bu sayede
//! **ortadan kalkar** — üç belirtinin (hiç algılamama, saniyelerce ölü
//! bölge, aim kayması) ortak kaynağı buydu. Ayrıntı: `docs/fare-nisan.md`.
//!
//! # Ayrıcalık
//!
//! Boruyu hwcomposer `mkfifo(..., 0660)` + `chown(..., 1000, 1000)` ile
//! kurar; sahibi Android'in `system` kullanıcısıdır. Host'tan yazmak için
//! root gerekir. Bu yüzden arka uç dosyayı KENDİSİ AÇMAZ: açık bir
//! tanıtıcı alır. `liwd-helper` boruyu açıp fd'yi D-Bus üzerinden verir,
//! böylece 200 Hz'lik yazma trafiği IPC'den geçmez.

use super::backend::{BackendError, TouchBackend};
use super::touch::{TouchAction, MAX_POINTERS};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// Konteyner içindeki dokunuş borusunun yolu.
pub const TOUCH_PIPE: &str = "dev/input/wl_touch_events";

/// hwcomposer'ın her dokunuşa yazdığı basınç. Aynısını kullanıyoruz:
/// farklı bir değer bazı oyunların basınca duyarlı davranışını değiştirir.
const PRESSURE: i32 = 50;

// evdev sabitleri. `evdev` kütüphanesi bunları tip olarak sunuyor ama
// buradaki tel formatı ham sayıdır; dönüştürmek yerine doğrudan yazıyoruz.
const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MT_PRESSURE: u16 = 0x3a;

/// `struct input_event`in tel üzerindeki boyutu (x86_64/aarch64).
/// `timeval` (2 × i64) + type + code + value.
const EVENT_SIZE: usize = 24;

/// POSIX yalnızca bu boyuta kadarki yazmaların bölünmezliğini garanti eder.
///
/// Şart: aynı boruya hwcomposer da yazıyor. Bir kareyi ikiye bölersek
/// araya onun kaydı girer ve EventHub bozuk `input_event` okur.
const PIPE_BUF: usize = 4096;

/// Waydroid konteynerinin dokunuş borusu.
#[derive(Debug)]
pub struct WlTouchBackend {
    pipe: File,
    /// Android ekran çözünürlüğü (`waydroid.display_width`/`height`).
    /// Koordinat uzayı budur; EventHub eksen bilgisini bu property'lerden
    /// üretir, cihazdan sormaz.
    w: u32,
    h: u32,
    /// Hangi slot kullanımda. Kalkışta `release_all` için gerekli.
    slots: [bool; MAX_POINTERS],
    /// Boru dolduğu için düşen kare sayısı (teşhis).
    dropped: u64,
}

impl WlTouchBackend {
    /// Açılmış bir boru tanıtıcısından kurar.
    ///
    /// Tanıtıcının `O_WRONLY | O_NONBLOCK` açılmış olması beklenir:
    /// bloklamak girdi döngüsünü kilitler ve fare tamamen durur. Boru
    /// dolarsa kareyi düşürmek doğrudur — Android geride kalmıştır ve
    /// eski konumu göndermenin değeri yoktur.
    pub fn from_pipe(pipe: File, w: u32, h: u32) -> Result<Self, BackendError> {
        if w == 0 || h == 0 {
            return Err(BackendError::Init(
                "ekran boyutu 0 — waydroid.display_width/height okunamadı".into()));
        }
        Ok(Self { pipe, w, h, slots: [false; MAX_POINTERS], dropped: 0 })
    }

    /// Konteynerin dokunuş borusunun host'tan görünen yolu.
    ///
    /// `nsenter` gerekmiyor: root, `/proc/<pid>/root/...` üzerinden başka
    /// bir mount namespace'indeki dosyayı doğrudan açabilir.
    pub fn pipe_path(container_pid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{container_pid}/root/{TOUCH_PIPE}"))
    }

    /// Düşen kare sayısı (teşhis).
    pub fn dropped(&self) -> u64 { self.dropped }

    /// Normalize koordinatı Android ekran pikseline çevirir.
    ///
    /// **Kırpma YOK.** Sınırsız nişanın tüm dayanağı bu; `Norm::unclamped`
    /// ile gelen ekran dışı değerler olduğu gibi geçmelidir.
    fn to_px(&self, at: super::touch::Norm) -> (i32, i32) {
        at.to_px(self.w, self.h)
    }
}

/// CLOCK_MONOTONIC saniye/mikrosaniye.
///
/// Duvar saati DEĞİL: Android girdi hattı olay zaman damgasını yeniden
/// örnekleme ve hız takibi için kullanır. Yanlış saat, teknik olarak
/// "çalışan" ama titreyen bir fare demektir.
fn monotonic() -> (i64, i64) {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: geçerli bir timespec'e yazıyoruz; clock_gettime başka bir
    // yan etki üretmez.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as i64, (ts.tv_nsec / 1000) as i64)
}

/// Tek bir `input_event` kaydını tel formatına yazar.
fn push_event(buf: &mut Vec<u8>, sec: i64, usec: i64, kind: u16, code: u16, value: i32) {
    buf.extend_from_slice(&sec.to_ne_bytes());
    buf.extend_from_slice(&usec.to_ne_bytes());
    buf.extend_from_slice(&kind.to_ne_bytes());
    buf.extend_from_slice(&code.to_ne_bytes());
    buf.extend_from_slice(&value.to_ne_bytes());
}

impl TouchBackend for WlTouchBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        if actions.is_empty() { return Ok(()); }
        let (sec, usec) = monotonic();
        let mut buf: Vec<u8> = Vec::with_capacity((actions.len() * 5 + 1) * EVENT_SIZE);

        for act in actions {
            match *act {
                TouchAction::Down { id, at } | TouchAction::Move { id, at } => {
                    let slot = id as usize;
                    if slot >= MAX_POINTERS {
                        return Err(BackendError::Dispatch(
                            format!("geçersiz işaretçi kimliği {id}")));
                    }
                    self.slots[slot] = true;
                    let (x, y) = self.to_px(at);
                    // hwcomposer'ın birebir sırası. İniş ve hareket aynı
                    // dizidir: protokol B'de yeni tracking_id inişi,
                    // aynısının tekrarı hareketi belirtir.
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_SLOT, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_TRACKING_ID, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_POSITION_X, x);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_POSITION_Y, y);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_PRESSURE, PRESSURE);
                }
                TouchAction::Up { id } => {
                    let slot = id as usize;
                    if slot >= MAX_POINTERS { continue }
                    self.slots[slot] = false;
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_SLOT, slot as i32);
                    push_event(&mut buf, sec, usec, EV_ABS, ABS_MT_TRACKING_ID, -1);
                }
            }
        }
        // Tek SYN_REPORT: bu çağrıdaki eylemler aynı kareye ait.
        push_event(&mut buf, sec, usec, EV_SYN, SYN_REPORT, 0);

        if buf.len() > PIPE_BUF {
            // Bölünmezliği kaybetmektense kareyi reddetmek iyidir: bozuk
            // kayıt Android'in girdi okuyucusunu hizadan çıkarır ve
            // sonrasında HİÇBİR dokunuş çalışmaz.
            return Err(BackendError::Dispatch(format!(
                "kare {} bayt — PIPE_BUF ({PIPE_BUF}) aşıldı, bölünmezlik kaybolurdu",
                buf.len())));
        }

        match self.pipe.write(&buf) {
            Ok(n) if n == buf.len() => Ok(()),
            Ok(n) => Err(BackendError::Dispatch(format!(
                "kısmi yazma: {n}/{} bayt — boru hizası bozuldu", buf.len()))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Android geride kalmış. Beklemek girdi döngüsünü kilitler;
                // düşürmek yalnızca bir kare kaybettirir.
                self.dropped += 1;
                Ok(())
            }
            Err(e) => Err(BackendError::Dispatch(format!("boruya yazılamadı: {e}"))),
        }
    }

    fn release_all(&mut self) -> Result<(), BackendError> {
        let ids: Vec<u8> = (0..MAX_POINTERS)
            .filter(|&i| self.slots[i]).map(|i| i as u8).collect();
        if ids.is_empty() { return Ok(()); }
        let acts: Vec<TouchAction> = ids.into_iter()
            .map(|id| TouchAction::Up { id }).collect();
        self.dispatch(&acts)
    }

    fn name(&self) -> &'static str { "wl_touch" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::touch::Norm;

    /// Tel formatı 24 bayt olmalı; Android bunu `sizeof(input_event)` ile
    /// bölerek okuyor. Yanlış boyut sessiz ve tam bozulma demek.
    #[test]
    fn event_record_is_24_bytes() {
        let mut b = Vec::new();
        push_event(&mut b, 1, 2, EV_ABS, ABS_MT_SLOT, 3);
        assert_eq!(b.len(), EVENT_SIZE);
        assert_eq!(std::mem::size_of::<libc::timeval>() + 8, EVENT_SIZE,
                   "timeval + type/code/value tel boyutuyla uyuşmalı");
    }

    #[test]
    fn pipe_path_reaches_into_the_container() {
        assert_eq!(WlTouchBackend::pipe_path(1234),
                   PathBuf::from("/proc/1234/root/dev/input/wl_touch_events"));
    }

    /// Ekran dışı koordinat KIRPILMAMALI — sınırsız nişanın tek dayanağı.
    #[test]
    fn offscreen_coordinates_survive_conversion() {
        let (_p, f) = tempfile("offscreen");
        let b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        assert_eq!(b.to_px(Norm::unclamped(1.5, 0.5)), (3840, 720));
        assert_eq!(b.to_px(Norm::unclamped(-0.25, 0.5)), (-640, 720));
    }

    #[test]
    fn zero_screen_is_rejected_loudly() {
        let e = WlTouchBackend::from_pipe(tempfile("zero").1, 0, 1440).unwrap_err();
        assert!(e.to_string().contains("display_width"), "{e}");
    }

    /// Bir kare TEK yazma olmalı ve `SYN_REPORT` ile bitmeli.
    #[test]
    fn a_frame_ends_with_one_syn_report() {
        let (p, f) = tempfile("frame");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[
            TouchAction::Down { id: 0, at: Norm::new(0.5, 0.5) },
            TouchAction::Move { id: 1, at: Norm::new(0.2, 0.3) },
        ]).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw.len() % EVENT_SIZE, 0, "kayıt hizası bozulmamalı");
        let n = raw.len() / EVENT_SIZE;
        assert_eq!(n, 11, "2 eylem × 5 olay + 1 SYN");
        let last = &raw[(n - 1) * EVENT_SIZE..];
        assert_eq!(u16::from_ne_bytes([last[16], last[17]]), EV_SYN);
        assert_eq!(u16::from_ne_bytes([last[18], last[19]]), SYN_REPORT);
    }

    /// Kalkış `tracking_id = -1` yazmalı; başka her değer parmağı ekranda
    /// bırakır.
    #[test]
    fn up_writes_tracking_id_minus_one() {
        let (p, f) = tempfile("up");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[TouchAction::Up { id: 3 }]).unwrap();
        let raw = std::fs::read(&p).unwrap();
        // slot, tracking_id, syn
        assert_eq!(raw.len() / EVENT_SIZE, 3);
        let tid = &raw[EVENT_SIZE..2 * EVENT_SIZE];
        assert_eq!(u16::from_ne_bytes([tid[18], tid[19]]), ABS_MT_TRACKING_ID);
        assert_eq!(i32::from_ne_bytes([tid[20], tid[21], tid[22], tid[23]]), -1);
    }

    /// `release_all` yalnızca GERÇEKTEN inen parmakları kaldırmalı.
    ///
    /// İkinci çağrının sessiz kalması önemli: temiz çıkışta ve profil
    /// değişiminde art arda çağrılıyor, her seferinde parmak kaldırmak
    /// oyuna sahte dokunuş sonları gönderirdi.
    #[test]
    fn release_all_lifts_only_active_slots() {
        let (p, f) = tempfile("release");
        let mut b = WlTouchBackend::from_pipe(f, 2560, 1440).unwrap();
        b.dispatch(&[TouchAction::Down { id: 2, at: Norm::new(0.5, 0.5) }]).unwrap();
        let after_down = std::fs::metadata(&p).unwrap().len() as usize;

        b.release_all().unwrap();
        let after_release = std::fs::metadata(&p).unwrap().len() as usize;
        assert_eq!((after_release - after_down) / EVENT_SIZE, 3,
                   "tek parmak: slot + tracking_id + syn");

        b.release_all().unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len() as usize, after_release,
                   "ikinci çağrı yeni olay üretmemeli");
    }

    // --- test yardımcıları ---
    // Gerçek FIFO yerine düz dosya: yazılan tel formatını okuyabiliyoruz.
    // Ad teste özgü: testler aynı süreçte PARALEL koşuyor, ortak dosya
    // kullanmak birbirlerinin çıktısını okumalarına yol açar.
    fn tempfile(tag: &str) -> (std::path::PathBuf, File) {
        let p = std::env::temp_dir()
            .join(format!("liw-wl-touch-{}-{tag}.bin", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let f = File::options().create(true).write(true).truncate(true)
            .open(&p).unwrap();
        (p, f)
    }
}
