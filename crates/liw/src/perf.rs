//! `liw perf` — performans kaldıraçlarının teşhisi.
//!
//! Bu komut HİÇBİR ŞEYİ DEĞİŞTİRMEZ. Sistemin halini okur ve nerede
//! performans bırakıldığını söyler. Uygulama ayrı bir adım.

use anyhow::Result;
use liw_core::perf::{self, Finding, Impact, Status, VirglClient};
use std::process::Command;

/// Bir dosyayı okur; yoksa boş dize döner (kaldıraç "yok" sayılır).
fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Komutu çalıştırıp stdout'unu döndürür. Başarısızsa boş dize.
///
/// Çıkış kodu KONTROL EDİLİYOR: başarısız komutun stdout'unu geçerli veri
/// saymak daha önce sessiz yanlış teşhise yol açmıştı.
fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Sürecin saniye cinsinden yaşı. Yoksa 0.
fn process_age(pattern: &str) -> u64 {
    run("pgrep", &["-f", pattern]).lines().next()
        .map(|pid| run("ps", &["-o", "etimes=", "-p", pid.trim()]))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// virgl istemci ağaçlarının köklerini bulur.
///
/// Ağaç yapısı: ana `virgl_test_server` her istemci için bir çocuk
/// çatallar, o da render sunucularını doğurur. İstemci sayısı ANA
/// SÜRECİN DOĞRUDAN ÇOCUKLARI kadardır — toplam süreç sayısı değil.
fn virgl_clients() -> Vec<VirglClient> {
    let raw = run("ps", &["-eo", "pid=,ppid=,etimes=,args="]);
    let rows: Vec<(u32, u32, u64)> = raw.lines()
        .filter(|l| l.contains("virgl_test_server"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?,
                  it.next()?.parse().ok()?))
        })
        .collect();

    // Ana süreç: virgl_test_server olmayan bir ata tarafından doğurulan.
    let pids: Vec<u32> = rows.iter().map(|(p, _, _)| *p).collect();
    let masters: Vec<u32> = rows.iter()
        .filter(|(_, pp, _)| !pids.contains(pp))
        .map(|(p, _, _)| *p).collect();

    rows.iter()
        .filter(|(_, pp, _)| masters.contains(pp))
        .map(|(pid, _, age_s)| VirglClient { pid: *pid, age_s: *age_s })
        .collect()
}

pub fn status() -> Result<()> {
    const CPU: &str = "/sys/devices/system/cpu/cpu0/cpufreq";

    let refresh = perf::parse_active_refresh(&run("kscreen-doctor", &["-o"]));
    // Oturumun grafik yaşı SurfaceFlinger'dan alınıyor: bu oturumun GPU
    // istemcileri ondan sonra doğmak zorunda.
    let session_age = process_age("surfaceflinger");

    let findings = vec![
        perf::governor(
            &read(&format!("{CPU}/scaling_governor")),
            &read(&format!("{CPU}/scaling_available_governors")),
            &read(&format!("{CPU}/scaling_driver"))),
        perf::epp(&read(&format!("{CPU}/energy_performance_preference"))),
        perf::powermizer(&run("nvidia-settings",
            &["-q", "[gpu:0]/GPUPowerMizerMode", "-t"])),
        perf::refresh_budget(refresh.unwrap_or(0.0)),
        perf::orphan_clients(&virgl_clients(), session_age),
        perf::container_weight(&run("systemctl",
            &["show", "waydroid-container", "-p", "CPUWeight"])),
    ];

    report(&findings);
    Ok(())
}

fn report(findings: &[Finding]) {
    println!("\n  Performans teşhisi\n  {}", "─".repeat(60));
    for f in findings {
        let (mark, label) = match f.status {
            Status::Optimal => ("✓", "hedefte"),
            Status::Improvable => ("!", "iyileştirilebilir"),
            Status::Unavailable => ("·", "bu makinede yok"),
        };
        println!("\n  {mark} {}  [{label}]", f.title);
        println!("      şu an : {}", f.current);
        if f.status == Status::Improvable {
            println!("      hedef : {}", f.target);
        }
        if f.impact == Impact::Unknown && f.status == Status::Improvable {
            println!("      etki  : ÖLÇÜLMEDİ");
        }
        for line in wrap(&f.note, 66) {
            println!("      {line}");
        }
    }

    let (ok, imp, na) = perf::summarise(findings);
    println!("\n  {}", "─".repeat(60));
    println!("  {ok} hedefte · {imp} iyileştirilebilir · {na} yok\n");
    if imp > 0 {
        println!("  Hiçbirinin etkisi bu sistemde ÖLÇÜLMEDİ. Uygulamadan önce");
        println!("  taban çizgisi al:\n");
        println!("      liw bench <paket> --duration 60\n");
    }
}

/// Notu sözcük sınırından katlar.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for w in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() { line.push(' '); }
        line.push_str(w);
    }
    if !line.is_empty() { out.push(line); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_respects_width_and_keeps_all_words() {
        let s = "bir iki üç dört beş altı yedi sekiz dokuz on";
        let lines = wrap(s, 12);
        assert!(lines.iter().all(|l| l.chars().count() <= 12), "{lines:?}");
        assert_eq!(lines.join(" "), s, "sözcük kaybolmamalı");
    }

    #[test]
    fn wrap_handles_empty() {
        assert!(wrap("", 20).is_empty());
    }

    /// Tek sözcük genişlikten uzunsa yine de kaybolmamalı.
    #[test]
    fn wrap_keeps_overlong_word() {
        assert_eq!(wrap("aşırıuzunbirsözcük", 5), vec!["aşırıuzunbirsözcük"]);
    }

    #[test]
    fn missing_file_reads_as_empty_not_panic() {
        assert_eq!(read("/proc/kesinlikle/olmayan/dosya"), "");
    }
}
