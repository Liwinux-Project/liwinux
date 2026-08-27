//! Performans kaldıraçlarının teşhisi.
//!
//! İlke: ÖNCE ÖLÇ, SONRA UYGULA. Bu modül hiçbir şeyi değiştirmez; sadece
//! sistemin şu anki halini okur ve nerede performans bırakıldığını söyler.
//! Uygulama ayrı bir adım ve ayrı bir onay ister.
//!
//! Buradaki her fonksiyon SAF: ham metin alır, bulgu döndürür. Böylece
//! root olmadan, gerçek donanım olmadan test edilebiliyor.

use serde::{Deserialize, Serialize};

/// Bir kaldıracın ölçülen etkisi.
///
/// Bu değerler TAHMİN değil, ölçülene kadar `Unknown`. Ölçmeden "yüksek
/// etki" demek tam da kaçındığımız şey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Impact {
    /// Ölçüldü, kare süresinde belirgin fark yarattı.
    Measured,
    /// Henüz ölçülmedi. Varsayılan durum.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Zaten hedefte.
    Optimal,
    /// Hedefte değil, iyileştirilebilir.
    Improvable,
    /// Bu makinede bu kaldıraç yok.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: &'static str,
    pub title: &'static str,
    pub current: String,
    pub target: String,
    pub status: Status,
    pub impact: Impact,
    /// Neden önemli — ya da neden önemsiz olabileceği.
    pub note: String,
}

impl Finding {
    fn unavailable(id: &'static str, title: &'static str, why: &str) -> Self {
        Self { id, title, current: "yok".into(), target: "-".into(),
            status: Status::Unavailable, impact: Impact::Unknown, note: why.into() }
    }
}

/// CPU frekans yöneticisi.
///
/// `intel_pstate` sürücüsünde `powersave` isminin yanıltıcı olduğunu
/// belirtmek gerekiyor: bu mod frekansı yine tavana çıkarır, `performance`
/// ile farkı çoğunlukla küçüktür. Bu yüzden ölçmeden büyük kazanç vaat
/// etmiyoruz.
pub fn governor(current: &str, available: &str, driver: &str) -> Finding {
    let cur = current.trim();
    if cur.is_empty() {
        return Finding::unavailable("cpu.governor", "CPU yöneticisi",
            "cpufreq arayüzü bulunamadı");
    }
    let has_perf = available.split_whitespace().any(|g| g == "performance");
    let pstate = driver.trim() == "intel_pstate";
    let note = if pstate {
        "intel_pstate'te 'powersave' yine tavan frekansa çıkar; fark \
         genelde küçük. Ölçmeden kazanç vaat edilmiyor."
    } else {
        "Talep tabanlı yöneticiler frekansı geç yükseltir; oyunda kare \
         gecikmesi yaratabilir."
    };
    Finding {
        id: "cpu.governor", title: "CPU yöneticisi",
        current: cur.to_string(),
        target: if has_perf { "performance".into() } else { "-".into() },
        status: if cur == "performance" { Status::Optimal }
                else if has_perf { Status::Improvable }
                else { Status::Unavailable },
        impact: Impact::Unknown, note: note.into(),
    }
}

/// Enerji/performans tercihi (yalnızca intel_pstate + HWP).
pub fn epp(current: &str) -> Finding {
    let cur = current.trim();
    if cur.is_empty() {
        return Finding::unavailable("cpu.epp", "Enerji/performans tercihi",
            "HWP EPP bu CPU'da açık değil");
    }
    Finding {
        id: "cpu.epp", title: "Enerji/performans tercihi",
        current: cur.to_string(), target: "performance".into(),
        status: if cur == "performance" { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "EPP, governor'dan daha doğrudan etki eder: turbo'nun ne kadar \
               agresif tutulacağını belirler.".into(),
    }
}

/// NVIDIA PowerMizer kipi. 0 = uyarlamalı, 1 = azami performans tercihi.
pub fn powermizer(raw: &str) -> Finding {
    let cur = raw.trim();
    let Ok(mode) = cur.parse::<i32>() else {
        return Finding::unavailable("gpu.powermizer", "NVIDIA PowerMizer",
            "nvidia-settings sorgulanamadı (Wayland'de X sunucusu gerekebilir)");
    };
    Finding {
        id: "gpu.powermizer", title: "NVIDIA PowerMizer",
        current: format!("{mode} ({})", match mode {
            0 => "uyarlamalı", 1 => "azami performans",
            2 => "otomatik", _ => "bilinmiyor" }),
        target: "1 (azami performans)".into(),
        status: if mode == 1 { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "Uyarlamalı kipte saat hızı yüke göre iner/çıkar; geçişler \
               tek tük uzun kare üretebilir.".into(),
    }
}

/// Ekranın tazeleme hızı — oyunun hedef FPS'ini bu belirliyor.
///
/// Yüksek tazeleme KÖTÜ DEĞİL. Burada bulgu olarak gösteriliyor çünkü
/// oyun kare bütçesini bu belirliyor: 180 Hz'de bütçe 5,6 ms, 60 Hz'de
/// 16,7 ms. Kare kaçırılıyorsa düşürmek tutarlılık kazandırır; kaçmıyorsa
/// dokunmaya gerek yok.
pub fn refresh_budget(hz: f64) -> Finding {
    let budget = if hz > 0.0 { 1000.0 / hz } else { 0.0 };
    Finding {
        id: "display.refresh", title: "Ekran tazeleme / kare bütçesi",
        current: format!("{hz:.0} Hz — kare başına {budget:.1} ms"),
        target: "ölçüme bağlı".into(),
        status: Status::Optimal,
        impact: Impact::Unknown,
        note: "Yüksek tazeleme kendiliğinden sorun değil. Yalnızca kare \
               kaçırma oranı yüksekse düşürmek tutarlılık kazandırır.".into(),
    }
}

/// `kscreen-doctor -o` çıktısından etkin çıkışın tazeleme hızını bulur.
///
/// Etkin mod satırda `*` ile işaretli. Birden fazla ekran varsa EN YÜKSEK
/// tazelemeli etkin mod alınır: oyun orada çalışıyor olacak.
pub fn parse_active_refresh(raw: &str) -> Option<f64> {
    let mut best: Option<f64> = None;
    for tok in raw.split_whitespace() {
        // Renk kaçış dizileri temizlenir; "2560x1440@180.00*" aranır.
        let t: String = strip_ansi(tok);
        if !t.contains('*') { continue; }
        let Some((_, hz)) = t.split_once('@') else { continue };
        let hz: String = hz.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if let Ok(v) = hz.parse::<f64>() {
            if best.is_none_or(|b| v > b) { best = Some(v); }
        }
    }
    best
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if c == '\u{1b}' { in_esc = true; continue; }
        if in_esc { if c.is_ascii_alphabetic() { in_esc = false; } continue; }
        out.push(c);
    }
    out
}

/// Bir virgl GPU istemcisi (bir ağacın kökü).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirglClient {
    pub pid: u32,
    pub age_s: u64,
}

/// Önceki oturumdan kalmış virgl istemcilerini bulur.
///
/// DİKKAT — burada bir kez yanlış teşhis kondu: süreç SAYISI ölçüt
/// sanıldı. Yanlış. Her Android süreci kendi GPU bağlamını açar, yani
/// tek oturumda ONLARCA istemci olması NORMAL.
///
/// Doğru ölçüt yaş karşılaştırması: bir istemci ancak SurfaceFlinger'dan
/// DAHA ESKİYSE önceki oturumdan kalmıştır, çünkü bu oturumun istemcileri
/// SurfaceFlinger'dan sonra doğmak zorunda.
///
/// `session_age_s` sıfırsa oturum çalışmıyordur; o zaman ayakta duran her
/// istemci artıktır.
pub fn orphan_clients(clients: &[VirglClient], session_age_s: u64) -> Finding {
    /// Doğum sırasındaki ölçüm gürültüsü için tolerans.
    const MARGIN_S: u64 = 5;

    let orphans = clients.iter()
        .filter(|c| c.age_s > session_age_s.saturating_add(MARGIN_S))
        .count();
    let live = clients.len() - orphans;

    Finding {
        id: "render.orphans", title: "Artık virgl istemcileri",
        current: if session_age_s == 0 {
            format!("{orphans} artık (oturum çalışmıyor)")
        } else {
            format!("{orphans} artık, {live} canlı ({} istemci)", clients.len())
        },
        target: "0".into(),
        status: if orphans > 0 { Status::Improvable } else { Status::Optimal },
        impact: Impact::Unknown,
        note: "Onlarca canlı istemci normaldir: her Android süreci kendi GPU \
               bağlamını açar. Yalnızca SurfaceFlinger'dan eski olanlar \
               önceki oturumdan kalmıştır.".into(),
    }
}

/// systemd biriminin CPU ağırlığı.
pub fn container_weight(raw: &str) -> Finding {
    let set = raw.lines()
        .find_map(|l| l.strip_prefix("CPUWeight="))
        .map(|v| v.trim())
        .filter(|v| *v != "[not set]" && !v.is_empty());
    Finding {
        id: "container.cpuweight", title: "Kap CPU ağırlığı",
        current: set.unwrap_or("ayarsız (varsayılan 100)").to_string(),
        target: "yüksek (ör. 300)".into(),
        status: if set.is_some() { Status::Optimal } else { Status::Improvable },
        impact: Impact::Unknown,
        note: "Yalnızca CPU DOLUYKEN etki eder. Boştaki sistemde farkı \
               ölçülemez — bu yüzden tek başına vaat edilmiyor.".into(),
    }
}

/// Bulgu listesini insan okunur tabloya çevirir.
pub fn summarise(findings: &[Finding]) -> (usize, usize, usize) {
    let mut ok = 0; let mut imp = 0; let mut na = 0;
    for f in findings {
        match f.status {
            Status::Optimal => ok += 1,
            Status::Improvable => imp += 1,
            Status::Unavailable => na += 1,
        }
    }
    (ok, imp, na)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_at_performance_is_optimal() {
        let f = governor("performance\n", "performance powersave", "intel_pstate");
        assert_eq!(f.status, Status::Optimal);
    }

    #[test]
    fn governor_powersave_is_improvable() {
        let f = governor("powersave\n", "performance powersave", "intel_pstate");
        assert_eq!(f.status, Status::Improvable);
        assert_eq!(f.target, "performance");
    }

    /// intel_pstate'te abartılı vaat vermemeliyiz; not bunu söylemeli.
    #[test]
    fn pstate_note_is_honest() {
        let f = governor("powersave", "performance powersave", "intel_pstate");
        assert!(f.note.contains("fark"), "not gerçekçi olmalı: {}", f.note);
    }

    #[test]
    fn missing_cpufreq_is_unavailable() {
        assert_eq!(governor("", "", "").status, Status::Unavailable);
        assert_eq!(epp("").status, Status::Unavailable);
    }

    #[test]
    fn powermizer_modes() {
        assert_eq!(powermizer("1").status, Status::Optimal);
        assert_eq!(powermizer("0").status, Status::Improvable);
        assert_eq!(powermizer("hata").status, Status::Unavailable);
    }

    /// Gerçek kscreen-doctor çıktısı renk kaçışlarıyla geliyor.
    #[test]
    fn parses_refresh_from_real_kscreen_output() {
        let raw = "\u{1b}[01;34mModes: \u{1b}[0;0m 25:2560x1440@60.00!  \
                   26:\u{1b}[01;32m2560x1440@180.00*\u{1b}[0;0m  27:2560x1440@165.00";
        assert_eq!(parse_active_refresh(raw), Some(180.0));
    }

    /// İki ekran varsa oyunun çalıştığı yüksek tazelemeli olan seçilmeli.
    #[test]
    fn picks_highest_active_refresh_across_outputs() {
        let raw = "1:1920x1080@60.00*  26:2560x1440@180.00*";
        assert_eq!(parse_active_refresh(raw), Some(180.0));
    }

    #[test]
    fn no_active_mode_yields_none() {
        assert_eq!(parse_active_refresh("1:1920x1080@60.00  2:800x600@75.00"), None);
    }

    #[test]
    fn refresh_budget_is_inverse_of_hz() {
        let f = refresh_budget(180.0);
        assert!(f.current.contains("5.6"), "bütçe yanlış: {}", f.current);
        // Yüksek tazeleme kusur olarak işaretlenmemeli.
        assert_eq!(f.status, Status::Optimal);
    }

    fn cl(pid: u32, age_s: u64) -> VirglClient { VirglClient { pid, age_s } }

    /// Gerçek ölçüm: 10 istemci, hepsi SurfaceFlinger'dan (40497s) genç.
    /// Bunlar CANLI — bir kez yanlışlıkla "artık" sanıldı.
    #[test]
    fn many_clients_younger_than_session_are_all_live() {
        let clients: Vec<_> = [40295, 40291, 40289, 40288, 40283, 40277,
                               40239, 39986, 40294, 377]
            .iter().enumerate().map(|(i, a)| cl(i as u32, *a)).collect();
        let f = orphan_clients(&clients, 40497);
        assert_eq!(f.status, Status::Optimal, "{}", f.current);
        assert!(f.current.contains("0 artık"), "{}", f.current);
    }

    /// Oturumdan ESKİ istemci gerçek artıktır.
    #[test]
    fn clients_older_than_session_are_orphans() {
        let f = orphan_clients(&[cl(1, 90000), cl(2, 100), cl(3, 50)], 1000);
        assert_eq!(f.status, Status::Improvable);
        assert!(f.current.contains("1 artık"), "{}", f.current);
    }

    /// Oturum yoksa ayakta duran her istemci artıktır.
    #[test]
    fn without_session_everything_is_orphan() {
        let f = orphan_clients(&[cl(1, 100), cl(2, 50)], 0);
        assert_eq!(f.status, Status::Improvable);
        assert!(f.current.contains("2 artık"), "{}", f.current);
    }

    #[test]
    fn no_clients_is_optimal() {
        assert_eq!(orphan_clients(&[], 1000).status, Status::Optimal);
    }

    #[test]
    fn unset_cpuweight_is_improvable() {
        let f = container_weight("CPUWeight=[not set]\nIOWeight=[not set]\n");
        assert_eq!(f.status, Status::Improvable);
        let f = container_weight("CPUWeight=300\n");
        assert_eq!(f.status, Status::Optimal);
    }

    /// Ölçülmemiş hiçbir kaldıraç "etkili" diye işaretlenmemeli.
    #[test]
    fn nothing_claims_measured_impact_before_measurement() {
        let all = [governor("powersave", "performance", "intel_pstate"),
                   epp("balance_performance"), powermizer("0"),
                   refresh_budget(180.0),
                   orphan_clients(&[cl(1, 90000)], 1000),
                   container_weight("CPUWeight=[not set]")];
        for f in &all {
            assert_eq!(f.impact, Impact::Unknown,
                "{} ölçülmeden etki iddia ediyor", f.id);
        }
        let (ok, imp, _) = summarise(&all);
        assert_eq!((ok, imp), (1, 5));
    }
}
