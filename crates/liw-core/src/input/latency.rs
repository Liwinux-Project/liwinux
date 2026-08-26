//! Gecikme ölçümü.
//!
//! Zincirin tamamı şu:
//!
//! ```text
//! tuş → çekirdek evdev → [BİZ] → uinput → libinput → KWin
//!     → wl_touch → Waydroid → Android girdi hattı → oyun
//! ```
//!
//! Yalnızca `[BİZ]` kısmını doğrudan ölçebiliyoruz: evdev olayının çekirdek
//! zaman damgası ile dokunuşu gönderdiğimiz an arasındaki fark. Geri kalanı
//! bu araçla göremeyiz ve **görüyormuş gibi yapmak yanlış olur** — bu yüzden
//! rapor neyi kapsayıp neyi kapsamadığını açıkça yazar.

use std::time::{Duration, SystemTime};

#[derive(Debug, Default)]
pub struct LatencyStats {
    samples_us: Vec<u64>,
    /// Zaman damgası geçmişte değil gelecekte görünen olaylar. Saat
    /// kayması varsa ölçüm anlamsızlaşır; sessizce sıfır saymak yerine sayıyoruz.
    pub skipped: usize,
}

impl LatencyStats {
    pub fn new() -> Self { Self::default() }

    /// evdev olayının zaman damgasından şu ana kadar geçen süreyi kaydeder.
    pub fn record(&mut self, event_time: SystemTime) {
        match SystemTime::now().duration_since(event_time) {
            Ok(d) => self.samples_us.push(d.as_micros() as u64),
            Err(_) => self.skipped += 1,
        }
    }

    pub fn record_duration(&mut self, d: Duration) {
        self.samples_us.push(d.as_micros() as u64);
    }

    pub fn len(&self) -> usize { self.samples_us.len() }
    pub fn is_empty(&self) -> bool { self.samples_us.is_empty() }

    fn pct(sorted: &[u64], p: f64) -> u64 {
        if sorted.is_empty() { return 0; }
        let i = ((sorted.len() as f64 - 1.0) * p / 100.0).round() as usize;
        sorted[i.min(sorted.len() - 1)]
    }

    /// (p50, p95, p99, en kötü) mikrosaniye.
    pub fn percentiles(&self) -> (u64, u64, u64, u64) {
        let mut s = self.samples_us.clone();
        s.sort_unstable();
        (Self::pct(&s, 50.0), Self::pct(&s, 95.0), Self::pct(&s, 99.0),
         *s.last().unwrap_or(&0))
    }

    pub fn mean_us(&self) -> u64 {
        if self.samples_us.is_empty() { return 0; }
        self.samples_us.iter().sum::<u64>() / self.samples_us.len() as u64
    }

    pub fn report(&self, label: &str) -> String {
        if self.is_empty() {
            // Atlananları burada da söyle: hepsi saat kayması yüzünden
            // atlandıysa "örnek yok" demek asıl nedeni gizler.
            return if self.skipped > 0 {
                format!("{label}: kullanılabilir örnek yok                          ({} örnek saat kayması nedeniyle atlandı)", self.skipped)
            } else {
                format!("{label}: örnek yok")
            };
        }
        let (p50, p95, p99, max) = self.percentiles();
        let mut s = format!(
            "{label}  ({} örnek)\n\
             \x20 p50 {:6.2} ms   p95 {:6.2} ms   p99 {:6.2} ms   en kötü {:6.2} ms   ort {:6.2} ms",
            self.len(),
            p50 as f64 / 1000.0, p95 as f64 / 1000.0, p99 as f64 / 1000.0,
            max as f64 / 1000.0, self.mean_us() as f64 / 1000.0);
        if self.skipped > 0 {
            s.push_str(&format!("\n  uyarı: {} örnek atlandı (saat kayması)", self.skipped));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_known_set() {
        let mut l = LatencyStats::new();
        for us in 1..=100u64 { l.record_duration(Duration::from_micros(us * 100)); }
        let (p50, p95, p99, max) = l.percentiles();
        assert_eq!(max, 10_000);
        assert!(p50 > 0 && p50 < p95 && p95 <= p99 && p99 <= max);
    }

    #[test]
    fn empty_stats_are_safe() {
        let l = LatencyStats::new();
        assert_eq!(l.percentiles(), (0, 0, 0, 0));
        assert_eq!(l.mean_us(), 0);
        assert!(l.report("x").contains("örnek yok"));
    }

    /// Gelecekten gelen zaman damgası sessizce sıfır sayılmamalı.
    #[test]
    fn future_timestamps_are_counted_not_swallowed() {
        let mut l = LatencyStats::new();
        l.record(SystemTime::now() + Duration::from_secs(10));
        assert_eq!(l.len(), 0);
        assert_eq!(l.skipped, 1);
        assert!(l.report("x").contains("saat kayması"));
    }

    #[test]
    fn single_sample_percentiles_are_that_sample() {
        let mut l = LatencyStats::new();
        l.record_duration(Duration::from_micros(2500));
        assert_eq!(l.percentiles(), (2500, 2500, 2500, 2500));
    }
}
