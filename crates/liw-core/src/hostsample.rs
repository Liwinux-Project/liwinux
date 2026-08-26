//! Host kaynak örneklemesi. Root gerektirmez.

use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, Default)]
pub struct HostSample {
    pub gpu_pct: f64,
    pub vram_mb: f64,
    pub cpu_pct: f64,
    pub ram_used_mb: f64,
    pub mem_pressure: f64,
}

/// `/proc/stat` farkından CPU kullanımı hesaplar.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuMeter {
    prev_total: u64,
    prev_idle: u64,
    /// İlk örnek alındı mı. Bayraksız, ilk çağrıda prev_* sıfır olduğu için
    /// fark "açılıştan beri geçen tüm süre" olur ve anlamsız bir yüzde çıkar.
    primed: bool,
}

impl CpuMeter {
    /// İlk çağrı 0 döner (fark yok); bu doğru davranış — uydurma bir
    /// değer vermek ilk örneği bozar.
    pub fn sample(&mut self, proc_stat_first_line: &str) -> f64 {
        let v: Vec<u64> = proc_stat_first_line.split_whitespace().skip(1)
            .filter_map(|x| x.parse().ok()).collect();
        if v.len() < 4 { return 0.0; }
        let idle = v[3];
        let total: u64 = v.iter().sum();
        let dt = total.saturating_sub(self.prev_total);
        let di = idle.saturating_sub(self.prev_idle);
        let primed = self.primed;
        self.prev_total = total;
        self.prev_idle = idle;
        self.primed = true;
        if !primed || dt == 0 { return 0.0; }
        100.0 * (dt - di) as f64 / dt as f64
    }
}

pub fn parse_meminfo_used_mb(meminfo: &str) -> f64 {
    let get = |k: &str| meminfo.lines()
        .find(|l| l.starts_with(k))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let total = get("MemTotal:");
    let avail = get("MemAvailable:");
    (total - avail) / 1024.0
}

pub fn parse_pressure_some_avg10(pressure: &str) -> f64 {
    pressure.lines()
        .find(|l| l.starts_with("some"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kv| kv.split('=').nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// `nvidia-smi` çıktısı: "39, 3493"
pub fn parse_nvidia(csv: &str) -> (f64, f64) {
    let mut it = csv.trim().split(',').map(|x| x.trim().parse::<f64>().unwrap_or(0.0));
    (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0))
}

pub async fn sample(cpu: &mut CpuMeter) -> HostSample {
    let stat = tokio::fs::read_to_string("/proc/stat").await.unwrap_or_default();
    let mem = tokio::fs::read_to_string("/proc/meminfo").await.unwrap_or_default();
    let psi = tokio::fs::read_to_string("/proc/pressure/memory").await.unwrap_or_default();
    let nv = Command::new("nvidia-smi")
        .args(["--query-gpu=utilization.gpu,memory.used",
               "--format=csv,noheader,nounits"])
        .stdin(Stdio::null()).stderr(Stdio::null())
        .output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let (gpu, vram) = parse_nvidia(&nv);
    HostSample {
        gpu_pct: gpu,
        vram_mb: vram,
        cpu_pct: cpu.sample(stat.lines().next().unwrap_or("")),
        ram_used_mb: parse_meminfo_used_mb(&mem),
        mem_pressure: parse_pressure_some_avg10(&psi),
    }
}

/// Örnek dizisinden ortalama ve tepe.
pub fn summarise(vals: &[f64]) -> (f64, f64) {
    if vals.is_empty() { return (0.0, 0.0); }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let max = vals.iter().cloned().fold(f64::MIN, f64::max);
    (mean, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_first_sample_is_zero_not_garbage() {
        let mut m = CpuMeter::default();
        let v = m.sample("cpu  100 0 100 800 0 0 0 0 0 0");
        assert_eq!(v, 0.0, "ilk örnekte fark yok, uydurma değer verilmemeli");
    }

    #[test]
    fn cpu_computes_busy_fraction() {
        let mut m = CpuMeter::default();
        m.sample("cpu  0 0 0 0 0 0 0 0 0 0");
        // 100 busy + 300 idle = 400 toplam -> %25
        let v = m.sample("cpu  100 0 0 300 0 0 0 0 0 0");
        assert!((v - 25.0).abs() < 0.01, "{v}");
    }

    #[test]
    fn meminfo_used_is_total_minus_available() {
        let m = "MemTotal:       16000000 kB\nMemAvailable:    6000000 kB\n";
        assert!((parse_meminfo_used_mb(m) - 9765.6).abs() < 1.0);
    }

    #[test]
    fn pressure_reads_some_avg10() {
        let p = "some avg10=1.75 avg60=0.30 avg300=0.05 total=123\n\
                 full avg10=0.50 avg60=0.10 avg300=0.01 total=45\n";
        assert!((parse_pressure_some_avg10(p) - 1.75).abs() < 1e-6);
    }

    #[test]
    fn nvidia_csv_parses() {
        assert_eq!(parse_nvidia("39, 3493"), (39.0, 3493.0));
    }

    #[test]
    fn missing_inputs_yield_zero_not_panic() {
        assert_eq!(parse_nvidia(""), (0.0, 0.0));
        assert_eq!(parse_meminfo_used_mb(""), 0.0);
        assert_eq!(parse_pressure_some_avg10(""), 0.0);
        assert_eq!(summarise(&[]), (0.0, 0.0));
    }

    #[test]
    fn summarise_mean_and_peak() {
        let (m, p) = summarise(&[10.0, 20.0, 60.0]);
        assert!((m - 30.0).abs() < 1e-9);
        assert!((p - 60.0).abs() < 1e-9);
    }
}
