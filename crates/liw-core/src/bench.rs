//! Kare zamanlaması ölçümü ve analizi.
//!
//! # Yöntem notu
//!
//! SurfaceFlinger 128 karelik YUVARLANAN bir tampon tutar. Bu iki tuzak doğurur
//! ve ikisine de bash prototipinde düştük:
//!
//! 1. **Örnekleme aralığı** tamponun dolma süresinden uzunsa kare kaybedilir.
//!    180 Hz'de 128 kare 0.71 sn'de dolar; 1 sn'lik sabit aralık kare kaçırır.
//!    Aralık refresh'ten TÜRETİLMELİ.
//! 2. **Pencere sınırı artefaktı**: iki anlık görüntü arasındaki boşluk gerçek
//!    bir kare aralığı DEĞİLDİR. Aralıklar yalnızca AYNI anlık görüntü
//!    içindeki ardışık karelerden hesaplanmalı; aksi halde "en kötü 500 ms"
//!    gibi uydurma değerler çıkar.

use std::collections::BTreeSet;

/// Geçersiz zaman damgası sınırı. SurfaceFlinger bilinmeyen kareler için
/// 0 veya INT64_MAX yazar.
const INVALID_MAX: i64 = i64::MAX / 2;

/// Tek bir `--latency` anlık görüntüsü.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Ekran yenileme periyodu (ns). Çıktının ilk satırı.
    pub refresh_ns: u64,
    /// Geçerli sunum zaman damgaları (ns), artan sırada, tekilleştirilmiş.
    pub presents: Vec<i64>,
}

/// `dumpsys SurfaceFlinger --latency <layer>` çıktısını ayrıştırır.
///
/// Biçim: ilk satır refresh periyodu, sonra üç sütunlu satırlar
/// (desiredPresentTime, actualPresentTime, frameReadyTime).
pub fn parse_latency(raw: &str) -> Option<Snapshot> {
    let mut lines = raw.lines().map(str::trim).filter(|l| !l.is_empty());
    let refresh_ns: u64 = lines.next()?.parse().ok()?;
    // Saçma refresh değerleri ayrıştırma hatasıdır; kabul etme.
    if !(1_000_000..=100_000_000).contains(&refresh_ns) { return None; }

    let mut presents = BTreeSet::new();
    for l in lines {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() != 3 { continue; }
        let Ok(actual) = cols[1].parse::<i64>() else { continue };
        if actual <= 0 || actual > INVALID_MAX { continue; }
        presents.insert(actual);
    }
    Some(Snapshot { refresh_ns, presents: presents.into_iter().collect() })
}

/// Toplanmış anlık görüntülerden kare aralıkları çıkarır.
#[derive(Debug, Default)]
pub struct FrameData {
    /// Aralıklar (ms), BAŞLANGIÇ KARESİNE göre tekilleştirilmiş.
    ///
    /// Anlık görüntüler üst üste biniyor: dumpsys 128 karelik tampon
    /// döndürüyor ama her örnekte yalnızca ~57 yeni kare oluyor, yani aynı
    /// aralık 2-3 örnekte birden görünüyor. Vec kullanmak onu 2-3 kez
    /// sayıyordu; ölçülen 7997 tekil kareden 23560 "aralık" çıkmıştı.
    ///
    /// Yüzdelikler bundan az etkilenir (pay ve payda birlikte şişer) ama
    /// örneklem sayısı olduğundan büyük görünür ve jank SAYILARI yanlış
    /// olur. Faz 4'te öncesi/sonrası karşılaştıracağımız için önemli.
    intervals: std::collections::BTreeMap<i64, f64>,
    /// Görülen tekil kareler; kapsam hesabı için.
    frames: BTreeSet<i64>,
    refresh_ns: u64,
}

impl FrameData {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, snap: &Snapshot) {
        if snap.refresh_ns > 0 { self.refresh_ns = snap.refresh_ns; }
        self.frames.extend(snap.presents.iter().copied());
        // Aralıklar pencere İÇİNDEN; sınır boşluğu asla kullanılmaz.
        for w in snap.presents.windows(2) {
            let d = (w[1] - w[0]) as f64 / 1e6;
            // Absürt değerleri ele: 0.05 ms altı ölçüm gürültüsü,
            // 1000 ms üstü duraklama (uygulama arka plana gitmiş olabilir).
            if (0.05..1000.0).contains(&d) {
                // Başlangıç karesi anahtarı: aynı aralık kaç örnekte
                // görünürse görünsün bir kez sayılır.
                self.intervals.insert(w[0], d);
            }
        }
    }

    pub fn interval_count(&self) -> usize { self.intervals.len() }
    pub fn frame_count(&self) -> usize { self.frames.len() }
    pub fn refresh_ms(&self) -> f64 { self.refresh_ns as f64 / 1e6 }

    /// Yakalama kapsamı: görülen kare / beklenen kare.
    ///
    /// Düşük kapsam sayıların temsili olmadığını gösterir — bunu raporlamak
    /// şart, yoksa eksik veriden çıkarılan sonuca güvenilir sanılır.
    pub fn coverage_pct(&self) -> f64 {
        if self.frames.len() < 2 || self.refresh_ns == 0 { return 0.0; }
        let first = *self.frames.iter().next().unwrap();
        let last = *self.frames.iter().next_back().unwrap();
        let span_ns = (last - first) as f64;
        let expected = span_ns / self.refresh_ns as f64;
        if expected <= 0.0 { return 0.0; }
        100.0 * self.frames.len() as f64 / expected
    }

    /// Sıralı aralıklar (yüzdelik hesabı için).
    fn sorted(&self) -> Vec<f64> {
        let mut v: Vec<f64> = self.intervals.values().copied().collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    }

    pub fn percentile(&self, p: f64) -> f64 {
        let v = self.sorted();
        if v.is_empty() { return 0.0; }
        let i = ((v.len() as f64 - 1.0) * p / 100.0).round() as usize;
        v[i.min(v.len() - 1)]
    }

    pub fn mean_ms(&self) -> f64 {
        if self.intervals.is_empty() { return 0.0; }
        self.intervals.values().sum::<f64>() / self.intervals.len() as f64
    }

    /// Refresh'in `mult` katından uzun aralıklar (jank).
    pub fn jank_count(&self, mult: f64) -> usize {
        let t = self.refresh_ms() * mult;
        if t <= 0.0 { return 0; }
        self.intervals.values().filter(|&&x| x > t).count()
    }

    pub fn jank_pct(&self, mult: f64) -> f64 {
        if self.intervals.is_empty() { return 0.0; }
        100.0 * self.jank_count(mult) as f64 / self.intervals.len() as f64
    }
}

/// Örnekleme aralığını refresh'ten türetir.
///
/// Tampon 128 kare; güvenlik payıyla dolmadan örnekliyoruz. Sabit bir değer
/// kullanmak yüksek yenileme hızlarında kare kaybettirir.
pub fn sample_interval_ms(refresh_ns: u64) -> u64 {
    const BUFFER_FRAMES: f64 = 128.0;
    const SAFETY: f64 = 0.45;
    let refresh_ms = refresh_ns as f64 / 1e6;
    let v = BUFFER_FRAMES * refresh_ms * SAFETY;
    v.clamp(80.0, 1000.0) as u64
}

#[cfg(test)]
mod tests {
    /// Üst üste binen anlık görüntüler aynı aralığı ÇOK KEZ saymamalı.
    ///
    /// dumpsys 128 karelik tampon döndürüyor ama her örnekte ~57 yeni kare
    /// var; aynı aralık 2-3 örnekte birden görünüyor. Gerçek ölçümde 7997
    /// tekil kareden 23560 "aralık" çıkmıştı.
    #[test]
    fn overlapping_snapshots_do_not_inflate_sample_count() {
        let mut fd = super::FrameData::new();
        let r = 5_555_555u64;
        let mk = |base: i64, n: i64| super::Snapshot {
            refresh_ns: r,
            presents: (0..n).map(|i| base + i * r as i64).collect(),
        };
        // Üç örnek, her biri bir öncekiyle büyük ölçüde örtüşüyor.
        fd.add(&mk(0, 10));
        fd.add(&mk(5 * r as i64, 10));
        fd.add(&mk(10 * r as i64, 10));
        // Kareler 0..20 -> 19 tekil aralık. Şişmiş sayım 27 verirdi.
        assert_eq!(fd.frame_count(), 20);
        assert_eq!(fd.interval_count(), 19,
            "örtüşen örnekler aralığı tekrar saymamalı");
    }

    /// Tekilleştirme yüzdelikleri bozmamalı: hepsi aynı aralıksa p50 odur.
    #[test]
    fn dedupe_preserves_percentiles() {
        let mut fd = super::FrameData::new();
        let r = 5_555_555u64;
        for k in 0..5i64 {
            fd.add(&super::Snapshot {
                refresh_ns: r,
                presents: (0..8).map(|i| (k * 4 + i) * r as i64).collect(),
            });
        }
        let p50 = fd.percentile(50.0);
        assert!((p50 - 5.5555).abs() < 0.01, "p50 = {p50}");
    }

    use super::*;

    const REAL: &str = "5555555\n\
        100000000 100000000 100000000\n\
        105555555 105555555 105555555\n\
        111111110 111111110 111111110\n";

    #[test]
    fn parses_refresh_and_presents() {
        let s = parse_latency(REAL).unwrap();
        assert_eq!(s.refresh_ns, 5_555_555);
        assert_eq!(s.presents.len(), 3);
    }

    #[test]
    fn rejects_invalid_timestamps() {
        let raw = format!("5555555\n0 0 0\n1 {} 1\n100 200 300\n", i64::MAX);
        let s = parse_latency(&raw).unwrap();
        assert_eq!(s.presents, vec![200], "0 ve INT64_MAX elenmeli");
    }

    #[test]
    fn rejects_absurd_refresh() {
        assert!(parse_latency("42\n100 200 300\n").is_none());
        assert!(parse_latency("999999999999\n100 200 300\n").is_none());
    }

    #[test]
    fn deduplicates_repeated_frames() {
        let raw = "5555555\n1 1000000 1\n2 1000000 2\n3 2000000 3\n";
        assert_eq!(parse_latency(raw).unwrap().presents, vec![1_000_000, 2_000_000]);
    }

    /// Pencere sınırı artefaktı: iki anlık görüntü arasındaki boşluk
    /// aralık sayılmamalı. Bash prototipinde "en kötü 500 ms" bu yüzden çıkmıştı.
    #[test]
    fn gap_between_snapshots_is_never_an_interval() {
        let mut fd = FrameData::new();
        // İki görüntü, aralarında 1 saniyelik boşluk.
        fd.add(&Snapshot { refresh_ns: 5_555_555,
            presents: vec![0, 5_555_555, 11_111_110] });
        fd.add(&Snapshot { refresh_ns: 5_555_555,
            presents: vec![1_011_111_110, 1_016_666_665] });
        assert_eq!(fd.interval_count(), 3, "2+1 pencere-içi aralık olmalı");
        let worst = fd.percentile(100.0);
        assert!(worst < 10.0, "sınır boşluğu aralık sayılmamalı, en kötü={worst}");
    }

    #[test]
    fn percentiles_are_ordered() {
        let mut fd = FrameData::new();
        let mut p = vec![0i64];
        for i in 1..200 { p.push(p[i - 1] + 5_555_555); }
        fd.add(&Snapshot { refresh_ns: 5_555_555, presents: p });
        let (p50, p95, p99) = (fd.percentile(50.0), fd.percentile(95.0), fd.percentile(99.0));
        assert!(p50 <= p95 && p95 <= p99);
        assert!((p50 - 5.5555).abs() < 0.01, "p50 refresh'e yakın olmalı: {p50}");
    }

    #[test]
    fn jank_counts_long_intervals() {
        let mut fd = FrameData::new();
        // Üç normal kare, sonra bir gecikmeli kare.
        fd.add(&Snapshot { refresh_ns: 5_000_000, presents: vec![
            0, 5_000_000, 10_000_000, 30_000_000,
        ]});
        assert_eq!(fd.jank_count(1.5), 1, "20ms'lik aralık jank olmalı");
        assert!((fd.jank_pct(1.5) - 33.33).abs() < 0.1);
    }

    /// Örnekleme aralığı yenileme hızından TÜRETİLMELİ; sabit değer
    /// yüksek Hz'de kare kaybettirir.
    #[test]
    fn sample_interval_shrinks_at_high_refresh() {
        let at60 = sample_interval_ms(16_666_667);
        let at180 = sample_interval_ms(5_555_555);
        assert!(at180 < at60, "180Hz'de aralık daha kısa olmalı: {at180} vs {at60}");
        assert!(at180 >= 80, "alt sınır korunmalı");
        assert!(at60 <= 1000, "üst sınır korunmalı");
    }

    #[test]
    fn empty_data_is_safe() {
        let fd = FrameData::new();
        assert_eq!(fd.percentile(50.0), 0.0);
        assert_eq!(fd.mean_ms(), 0.0);
        assert_eq!(fd.coverage_pct(), 0.0);
        assert_eq!(fd.jank_count(1.5), 0);
    }

    #[test]
    fn coverage_reports_missing_frames() {
        let mut fd = FrameData::new();
        // 10 kare aralığı süren bir span'de yalnızca 6 kare görüldü.
        fd.add(&Snapshot { refresh_ns: 5_000_000, presents: vec![
            0, 5_000_000, 10_000_000, 15_000_000, 40_000_000, 50_000_000,
        ]});
        let c = fd.coverage_pct();
        assert!(c > 50.0 && c < 70.0, "kapsam ~60% olmalı, {c}");
    }
}
