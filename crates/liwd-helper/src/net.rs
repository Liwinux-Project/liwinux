//! Ağ teşhisi — `liw-net-doctor.sh`'ın Rust karşılığı.
//!
//! Faz 0'da öğrenilen ders: "firewall'unu kontrol et" demek işe yaramaz.
//! Gerçek arıza ufw kurallarımız DOĞRUYKEN ortaya çıktı; trafiği kaçıran şey
//! nftables'a kendi tablosunu yazan üçüncü bir araçtı (unwall/zapret, DNS'i
//! 127.0.0.53'e DNAT'lıyordu). Bu yüzden teşhis, **kuralı isimlendirmek**
//! zorundadır.

use serde::Serialize;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Serialize, Default)]
pub struct NetDiagnosis {
    pub bridge_up: bool,
    pub bridge_addr: Option<String>,
    pub dnsmasq_running: bool,
    pub ip_forward: bool,
    pub waydroid_nat: bool,
    pub lease_present: bool,
    pub active_firewall: String,
    /// waydroid0 trafiğini kaçıran YABANCI kurallar — tablo adıyla birlikte.
    pub hijack_rules: Vec<HijackRule>,
    pub problems: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HijackRule {
    pub table: String,
    pub rule: String,
    pub why: String,
}

async fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).stdin(Stdio::null()).output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

async fn ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args)
        .stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null())
        .status().await.map(|s| s.success()).unwrap_or(false)
}

async fn active_firewall() -> String {
    for (svc, name) in [("ufw", "ufw"), ("firewalld", "firewalld"),
                        ("nftables", "nftables"), ("iptables", "iptables")] {
        if ok("systemctl", &["is-active", "--quiet", svc]).await {
            return name.to_string();
        }
    }
    "none".to_string()
}

/// nftables'ta waydroid trafiğini kaçırabilecek yabancı kuralları bulur.
///
/// netfilter'da aynı hook'ta birden çok base chain çalışır ve **biri drop/dnat
/// derse** diğerlerinin accept'i bunu geçersiz kılamaz. Bu yüzden Waydroid'in
/// kendi kuralları doğruyken bile trafik ölebilir.
fn find_hijacks(ruleset: &str) -> Vec<HijackRule> {
    let mut found = Vec::new();
    let mut table = String::new();
    for line in ruleset.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("table ") {
            table = rest.trim_end_matches(" {").to_string();
        }
        // Waydroid'in kendi tabloları meşru; onları atla.
        if table.ends_with(" lxc") || table.ends_with("liwdiag") { continue; }
        if t.contains("dport 53") && t.contains("dnat to") {
            found.push(HijackRule {
                table: table.clone(),
                rule: t.to_string(),
                why: "DNS sorguları başka bir çözümleyiciye yönlendiriliyor; \
                      loopback hedefine giden dış paket reddedilir (ECONNREFUSED)".into(),
            });
        }
    }
    found
}

pub async fn diagnose() -> NetDiagnosis {
    let mut d = NetDiagnosis::default();

    let addr = out("ip", &["-4", "-o", "addr", "show", "waydroid0"]).await;
    d.bridge_up = !addr.trim().is_empty();
    d.bridge_addr = addr.split_whitespace().nth(3).map(str::to_string);

    d.dnsmasq_running = ok("pgrep", &["-f", "dnsmasq.*waydroid0"]).await;
    d.ip_forward = tokio::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
        .await.map(|s| s.trim() == "1").unwrap_or(false);
    d.lease_present = tokio::fs::metadata("/var/lib/misc/dnsmasq.waydroid0.leases")
        .await.map(|m| m.len() > 0).unwrap_or(false);
    d.active_firewall = active_firewall().await;

    let ruleset = out("nft", &["list", "ruleset"]).await;
    d.waydroid_nat = ruleset.contains("masquerade");
    d.hijack_rules = find_hijacks(&ruleset);

    if !d.bridge_up { d.problems.push("waydroid0 köprüsü yok".into()); }
    if !d.dnsmasq_running { d.problems.push("dnsmasq çalışmıyor".into()); }
    if !d.ip_forward { d.problems.push("ip_forward kapalı".into()); }
    if !d.waydroid_nat { d.problems.push("NAT/masquerade kuralı yok".into()); }
    if !d.lease_present { d.problems.push("DHCP lease yok — konteyner IP almamış".into()); }
    for h in &d.hijack_rules {
        d.problems.push(format!("'{}' tablosundaki kural DNS'i kaçırıyor: {}", h.table, h.rule));
    }
    d
}

#[cfg(test)]
mod tests {
    use super::find_hijacks;

    const REAL: &str = r#"
table ip unwall {
	chain dns_redirect {
		iifname != "enp3s0" udp dport 53 dnat to 127.0.0.53:53
	}
}
table inet lxc {
	chain input {
		iifname "waydroid0" udp dport { 53, 67 } accept
	}
}
"#;

    /// Faz 0'daki gerçek arıza: unwall'ın DNAT kuralı bulunmalı.
    #[test]
    fn finds_the_unwall_dns_hijack() {
        let h = find_hijacks(REAL);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].table, "ip unwall");
        assert!(h[0].rule.contains("dnat to 127.0.0.53"));
    }

    /// Waydroid'in kendi accept kuralları kaçırma sayılmamalı.
    #[test]
    fn waydroid_own_rules_are_not_hijacks() {
        let only_lxc = "table inet lxc {\n\tiifname \"waydroid0\" udp dport { 53, 67 } accept\n}\n";
        assert!(find_hijacks(only_lxc).is_empty());
    }

    #[test]
    fn clean_ruleset_reports_nothing() {
        assert!(find_hijacks("table ip filter {\n\tchain input {\n\t}\n}\n").is_empty());
    }
}
