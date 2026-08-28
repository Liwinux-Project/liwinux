//! Waydroid CLI wrapper.

use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum WaydroidError {
    #[error("could not run waydroid: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("waydroid '{cmd}' failed (code {code:?}): {stderr}")]
    Failed { cmd: String, code: Option<i32>, stderr: String },
    #[error("waydroid is not installed (not found on PATH)")]
    NotInstalled,
}

/// Parsed form of `waydroid status` output.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub session: String,
    pub container: String,
    pub vendor_type: String,
    pub ip: Option<String>,
    pub session_user: Option<String>,
}

impl Status {
    pub fn session_running(&self) -> bool {
        self.session.eq_ignore_ascii_case("RUNNING")
    }
    pub fn container_running(&self) -> bool {
        self.container.eq_ignore_ascii_case("RUNNING")
    }
    /// Has an IP been assigned? "UNKNOWN" means no IP — Waydroid prints that
    /// string rather than an empty one, so it must be filtered out separately.
    pub fn has_ip(&self) -> bool {
        matches!(self.ip.as_deref(), Some(s) if !s.is_empty() && s != "UNKNOWN")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Waydroid;

impl Waydroid {
    pub fn new() -> Self { Self }

    pub async fn available(&self) -> bool {
        Command::new("waydroid").arg("--version")
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status().await.map(|s| s.success()).unwrap_or(false)
    }

    /// Raw `waydroid <args>` invocation.
    ///
    /// CAREFUL: `waydroid shell` uses argparse and `COMMAND` is `nargs='*'`,
    /// so dashed arguments (`ls -la`, `sh -c`, `logcat -d`) get swallowed.
    /// That is why [`Waydroid::shell`] always inserts the `--` separator.
    async fn run(&self, args: &[&str]) -> Result<String, WaydroidError> {
        let out = Command::new("waydroid")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound {
                WaydroidError::NotInstalled
            } else {
                WaydroidError::Spawn(e)
            })?;
        if !out.status.success() {
            return Err(WaydroidError::Failed {
                cmd: args.join(" "),
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn status(&self) -> Result<Status, WaydroidError> {
        let raw = self.run(&["status"]).await?;
        Ok(parse_status(&raw))
    }

    /// Runs a command inside the container. The `--` separator is mandatory
    /// (see [`Waydroid::run`]). Requires root; an unprivileged call returns
    /// `Failed`.
    pub async fn shell(&self, argv: &[&str]) -> Result<String, WaydroidError> {
        let mut args = vec!["--details-to-stdout", "shell", "--"];
        args.extend_from_slice(argv);
        let raw = self.run(&args).await?;
        // `--details-to-stdout` also mixes in lxc-info noise; strip it.
        Ok(raw.lines()
            .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub async fn getprop(&self, key: &str) -> Result<String, WaydroidError> {
        Ok(self.shell(&["getprop", key]).await?.trim().to_string())
    }

    pub async fn session_stop(&self) -> Result<(), WaydroidError> {
        self.run(&["session", "stop"]).await.map(|_| ())
    }
}

fn parse_status(raw: &str) -> Status {
    let map: HashMap<&str, &str> = raw
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.trim_end_matches(':').trim(), v.trim()))
        .collect();
    Status {
        session: map.get("Session").unwrap_or(&"").to_string(),
        container: map.get("Container").unwrap_or(&"").to_string(),
        vendor_type: map.get("Vendor type").unwrap_or(&"").to_string(),
        ip: map.get("IP address").map(|s| s.to_string()),
        session_user: map.get("Session user").map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Session:\tRUNNING\nContainer:\tRUNNING\nVendor type:\tMAINLINE\nIP address:\t192.168.240.112\nSession user:\twintone01(1000)\nWayland display:\twayland-0\n";

    #[test]
    fn parses_running_status() {
        let s = parse_status(SAMPLE);
        assert!(s.session_running());
        assert!(s.container_running());
        assert_eq!(s.ip.as_deref(), Some("192.168.240.112"));
        assert!(s.has_ip());
    }

    #[test]
    fn stopped_status_has_no_container_line() {
        let s = parse_status("Session:\tSTOPPED\nVendor type:\tMAINLINE\n");
        assert!(!s.session_running());
        assert!(!s.container_running());
        assert!(!s.has_ip());
    }

    /// Waydroid reports a missing IP as "UNKNOWN" rather than an empty string.
    #[test]
    fn unknown_ip_is_not_an_ip() {
        let s = parse_status("Session:\tRUNNING\nIP address:\tUNKNOWN\n");
        assert!(s.session_running());
        assert!(!s.has_ip());
    }
}
