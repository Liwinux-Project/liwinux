//! liwd-helper — system service for privileged operations.
//!
//! # Security design
//!
//! This daemon runs as root and listens on the system bus. It therefore
//! **exposes NO general-purpose shell interface**: a method like `Shell(argv)`
//! would give every local user on the machine a path to root execution, even
//! behind polkit. Instead it offers narrow, named operations; each is bound to
//! its own polkit action and validates its inputs.

mod net;

use anyhow::Result;
use liw_core::{polkit_check, valid_prop_key};
use std::process::Stdio;
use tokio::process::Command;
use zbus::{connection, interface, message::Header, Connection};

const BUS_NAME: &str = "id.liwinux.Helper1";
const OBJ_PATH: &str = "/id/liwinux/Helper1";

const ACT_PROP: &str = "id.liwinux.helper.read-property";
const ACT_DIAG: &str = "id.liwinux.helper.net-diagnose";
const ACT_REPAIR: &str = "id.liwinux.helper.net-repair";
const ACT_OVERLAY: &str = "id.liwinux.helper.debug-overlay";
const ACT_FOREGROUND: &str = "id.liwinux.helper.foreground-app";
const ACT_PERF: &str = "id.liwinux.helper.performance";
const ACT_LOG: &str = "id.liwinux.helper.read-log";
const ACT_AUDIO: &str = "id.liwinux.helper.restart-audio";
const ACT_TOUCH: &str = "id.liwinux.helper.touch-pipe";

/// Full init path of the audio HAL. Fixed: taking a process name from the
/// caller would let them kill anything they liked.
const AUDIO_HAL: &str = "/vendor/bin/hw/android.hardware.audio.service";

/// Path of Waydroid's touch pipe inside the container.
///
/// Boruyu hwcomposer kurar (`mkfifo` 0660, `chown` system:system) ve
/// Android's patched `EventHub` listens on it as the `wayland_touch` device.
/// Writing here bypasses the compositor chain entirely; the rationale is in
/// `docs/mouse-aim.md`.
const TOUCH_PIPE: &str = "dev/input/wl_touch_events";

/// Name of a process inside the container — needed to reach the pipe via
/// `/proc/<pid>/root`. The container has its own mount namespace, but root can
/// reach into it without changing namespace.
///
/// `surfaceflinger` was chosen because it cannot exist outside the container,
/// and if it is dead there is no display to inject into anyway.
const CONTAINER_ANCHOR: &str = "surfaceflinger";

struct Helper {
    conn: Connection,
}

impl Helper {
    /// Android ekran boyutu (`waydroid.display_width`/`height`).
    ///
    /// NOT the host window size: the touch pipe's coordinate space is the
    /// Android display.
    ///
    /// `waydroid prop get` CANNOT BE USED: that command goes through
    /// `DBusSessionService` and
    /// connects to the session bus (`tools/actions/prop.py`). This service runs
    /// as root with no session bus — the command silently produced empty output
    /// and wrote "WayDroid session is stopped" to stderr. This actually
    /// happened: after a session restart the keymapper fell back to uinput every
    /// time and the user could not see why.
    ///
    /// `waydroid shell -- getprop` uses lxc-attach instead; that is the path
    /// that works as root.
    async fn display_px(&self) -> zbus::fdo::Result<(u32, u32)> {
        let w = shell_getprop("waydroid.display_width").await?;
        let h = shell_getprop("waydroid.display_height").await?;
        let parse = |v: String, key: &str| v.trim().parse::<u32>()
            .map_err(|_| zbus::fdo::Error::Failed(format!(
                "{key} is not a number: {v:?} — hwcomposer may not have \
                 hotplugged yet")));
        let (w, h) = (parse(w, "waydroid.display_width")?,
                      parse(h, "waydroid.display_height")?);
        if w == 0 || h == 0 {
            return Err(zbus::fdo::Error::Failed(
                "display size is 0 — hwcomposer has not hotplugged yet".into()));
        }
        Ok((w, h))
    }

    async fn authorize(&self, hdr: &Header<'_>, action: &str, interactive: bool)
        -> zbus::fdo::Result<()>
    {
        let caller = hdr.sender()
            .ok_or_else(|| zbus::fdo::Error::AuthFailed("no caller identity".into()))?;
        tracing::debug!(caller = %caller, action, interactive, "polkit sorgusu");
        match polkit_check(&self.conn, caller.as_str(), action, interactive).await {
            Ok(()) => {
                tracing::info!(caller = %caller, action, "yetki verildi");
                Ok(())
            }
            Err(e) => {
                // Record the distinction: did polkit DENY, or was polkit
                // UNREACHABLE? Very different problems; folding them into one
                // error makes diagnosis impossible.
                Err(zbus::fdo::Error::AccessDenied(format!("{e}")))
            }
        }
    }
}

/// `waydroid shell -- getprop <key>` — the property read path that works as root.
///
/// lxc noise is stripped from the output: `waydroid --details-to-stdout` mixes
/// lxc-info lines into stdout and they silently break number parsing.
async fn shell_getprop(key: &str) -> zbus::fdo::Result<String> {
    let out = Command::new("waydroid")
        .args(["--details-to-stdout", "shell", "--", "getprop", key])
        .stdin(Stdio::null())
        .output().await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(zbus::fdo::Error::Failed(format!(
            "getprop {key} failed (code {:?}): {err}", out.status.code())));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
        .collect::<Vec<_>>()
        .join(""))
}

/// Find a pid through which the container is visible.
async fn container_pid() -> Option<u32> {
    let out = Command::new("pgrep").args(["-x", CONTAINER_ANCHOR])
        .stdin(Stdio::null()).output().await.ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.parse().ok()
}

#[interface(name = "id.liwinux.Helper1")]
impl Helper {
    /// Opens Waydroid's touch pipe and hands over the WRITE handle.
    ///
    /// The handle is returned for speed: the keymapper writes ~200 frames per
    /// second and routing every frame through D-Bus would add latency and put
    /// this service in the middle of the input path. With a handle,
    /// authorization is asked once and the data never passes through here.
    ///
    /// The display size is returned too: pipe coordinates are directly in
    /// Android screen space and `EventHub` derives the axis range from those
    /// properties rather than asking the device. A caller using the host window
    /// yere dokunmak demek olurdu.
    async fn open_touch_pipe(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<(zbus::zvariant::OwnedFd, u32, u32)> {
        self.authorize(&hdr, ACT_TOUCH, false).await?;

        let pid = container_pid().await.ok_or_else(|| zbus::fdo::Error::Failed(
            "Waydroid container is not running (no surfaceflinger)".into()))?;
        let path = format!("/proc/{pid}/root/{TOUCH_PIPE}");

        let (w, h) = self.display_px().await?;

        // O_NONBLOCK serves two purposes at once:
        //  * With no reader, opening fails IMMEDIATELY with ENXIO. Blocking
        //    would have looked like "the keymapper hangs at startup".
        //  * When the pipe is full the write does not block; the caller drops
        //    the frame. Locking the input loop would stop the mouse entirely.
        let file = tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new().write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
            }
        }).await
          .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
          .map_err(|e| {
              let hint = if e.raw_os_error() == Some(libc::ENXIO) {
                  " — the pipe has no reader; Android's input reader has not \
                     open the pipe yet; try restarting the session"
              } else { "" };
              zbus::fdo::Error::Failed(format!("could not open {path}: {e}{hint}"))
          })?;

        tracing::info!(%path, width = w, height = h,
            "touch pipe handed over");
        Ok((std::os::fd::OwnedFd::from(file).into(), w, h))
    }

    /// Reads an Android property. The key's character set is validated.
    async fn get_prop(
        &self,
        key: &str,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        if !valid_prop_key(key) {
            return Err(zbus::fdo::Error::InvalidArgs(
                format!("invalid property key: {key:?}")));
        }
        self.authorize(&hdr, ACT_PROP, false).await?;
        // The "--" separator is mandatory: waydroid shell uses argparse and
        // would otherwise swallow dashed arguments.
        let out = Command::new("waydroid")
            .args(["--details-to-stdout", "shell", "--", "getprop", key])
            .stdin(Stdio::null())
            .output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        // Not checking the exit code turns a failure into an empty string and
        // tells the caller "the property is empty". Make the error visible.
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            tracing::warn!(key, code = ?out.status.code(), stderr = %err,
                           "waydroid shell failed");
            return Err(zbus::fdo::Error::Failed(format!(
                "waydroid shell failed (code {:?}): {}", out.status.code(), err)));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string())
    }

    /// Has Android finished booting? So `liwd` can measure rather than infer.
    async fn boot_completed(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<bool> {
        Ok(self.get_prop("sys.boot_completed", hdr).await?.trim() == "1")
    }

    /// Returns the package name of the foreground Android app.
    ///
    /// Automatic profile selection depends on this. The `dumpsys activity`
    /// output varies between Android versions, so several patterns are tried;
    /// if none match it returns an empty string (it does not invent one).
    async fn foreground_package(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_FOREGROUND, false).await?;
        let out = Command::new("waydroid")
            .args(["--details-to-stdout", "shell", "--",
                   "dumpsys", "activity", "activities"])
            .stdin(Stdio::null())
            .output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(zbus::fdo::Error::Failed(format!("dumpsys failed: {err}")));
        }
        Ok(parse_foreground(&String::from_utf8_lossy(&out.stdout)).unwrap_or_default())
    }

    /// SurfaceFlinger layer list (for measurement).
    async fn surface_layers(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_PERF, false).await?;
        run_dumpsys(&["dumpsys", "SurfaceFlinger", "--list"]).await
    }

    /// Frame timing data for one layer.
    ///
    /// The layer name goes straight into the command; we do not use a shell
    /// (exec, not shell) but control characters are filtered anyway — argument
    /// injection is not possible on this path, but validating input is cheap.
    async fn surface_latency(
        &self,
        layer: &str,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        if layer.is_empty() || layer.len() > 512
            || layer.chars().any(|c| c.is_control())
        {
            return Err(zbus::fdo::Error::InvalidArgs("invalid layer name".into()));
        }
        self.authorize(&hdr, ACT_PERF, false).await?;
        run_dumpsys(&["dumpsys", "SurfaceFlinger", "--latency", layer]).await
    }

    /// Toggles Android's touch indicator (pointer location).
    ///
    /// Mandatory for calibration: coordinate mapping cannot be tuned without
    /// seeing WHERE a touch lands. It changes only this developer setting;
    /// fixed command, no caller-supplied string.
    async fn set_pointer_location(
        &self,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.authorize(&hdr, ACT_OVERLAY, false).await?;
        let val = if enabled { "1" } else { "0" };
        for key in ["pointer_location", "show_touches"] {
            let out = Command::new("waydroid")
                .args(["--details-to-stdout", "shell", "--",
                       "settings", "put", "system", key, val])
                .stdin(Stdio::null())
                .output().await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(zbus::fdo::Error::Failed(
                    format!("settings put {key} failed: {err}")));
            }
        }
        tracing::info!(enabled, "touch indicator set");
        Ok(())
    }

    /// Read-only network diagnosis (JSON). Changes nothing, needs no interaction.
    /// The last lines of the Android log.
    ///
    /// The only way to diagnose app hangs. To avoid opening a general `Shell()`
    /// the arguments are TIGHTLY constrained: the buffer name is picked from a
    /// fixed list and the line count is bounded. No user text reaches the
    /// command line.
    async fn logcat(
        &self,
        buffer: &str,
        lines: u32,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let Some(buf) = valid_log_buffer(buffer) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid buffer: {buffer:?} (main|crash|system|events|all)")));
        };
        let n = clamp_log_lines(lines);
        self.authorize(&hdr, ACT_LOG, false).await?;
        let n = n.to_string();
        run_dumpsys(&["logcat", "-d", "-b", buf, "-t", &n]).await
    }

    /// logcat with MONOTONIC timestamps — for diagnostic correlation.
    ///
    /// A separate method from `Logcat` because a format difference is a meaning
    /// difference: the default output is wall clock while frame timestamps are
    /// `CLOCK_MONOTONIC`. Aligning them needed timezone and day-boundary
    /// guesswork; `-v monotonic` removes that entirely.
    async fn log_trace(
        &self,
        buffer: &str,
        lines: u32,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        let Some(buf) = valid_log_buffer(buffer) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid buffer: {buffer:?} (main|crash|system|events|all)")));
        };
        let n = clamp_log_lines(lines);
        self.authorize(&hdr, ACT_LOG, false).await?;
        let n = n.to_string();
        // hwcomposer is SILENCED — measured: it writes two lines per frame
        // (`attach dmabuf: ...`). At 180 Hz that is ~360 lines a second, and a
        // 400-line tail then covers only ONE second; the events we are looking
        // for (GC, Skipped frames, network timeouts) drop out of the ring
        // before we ever see them.
        //
        // `*:I` is mandatory: the moment any filter is given, logcat's default
        // becomes "silence everything". Without it the output would be empty.
        run_dumpsys(&["logcat", "-d", "-b", buf,
                      "-v", "monotonic", "-v", "threadtime", "-t", &n,
                      "hwcomposer:S", "*:I"]).await
    }

    /// Restarts a wedged audio HAL.
    ///
    /// Measured failure: once the HAL wedges, `audioserver` blocks in its
    /// `registerClient` call, the watchdog aborts it after 5 seconds, it
    /// restarts and wedges in the same place. Result: `AudioFlinger` never
    /// publishes and EVERY app that touches audio waits forever — which the
    /// user experiences as "the game will not open".
    ///
    /// Killing the HAL makes Android's init restart it immediately. Far less
    /// destructive than restarting the whole session.
    ///
    /// The process name is FIXED: taking it as a parameter would let the caller
    /// kill any process as root.
    async fn restart_audio(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_AUDIO, true).await?;
        let out = Command::new("pkill").args(["-f", AUDIO_HAL])
            .stdin(Stdio::null()).output().await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        // pkill: 0 = matched and signalled, 1 = no match.
        // Treating 1 as an error would be wrong, but saying "restarted" would be
        // a lie — it is reported separately.
        match out.status.code() {
            Some(0) => {
                tracing::info!("audio HAL restarted");
                Ok("audio HAL killed — init will restart it".into())
            }
            Some(1) => Ok("the audio HAL is not running".into()),
            c => Err(zbus::fdo::Error::Failed(format!("pkill failed: {c:?}"))),
        }
    }

    async fn net_diagnose(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_DIAG, false).await?;
        let d = net::diagnose().await;
        serde_json::to_string(&d).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Repairs firewall rules. Requires administrator authorization.
    ///
    /// It only ADDS missing rules; it removes nothing and does NOT change
    /// foreign tables that hijack DNS on its own — silently breaking another
    /// tool's configuration is unacceptable, so it reports instead.
    async fn net_repair(
        &self,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&hdr, ACT_REPAIR, true).await?;
        let d = net::diagnose().await;
        let mut done: Vec<String> = Vec::new();

        if d.active_firewall == "ufw" {
            for args in [
                vec!["allow", "in", "on", "waydroid0", "to", "any", "port", "67", "proto", "udp",
                     "comment", "liwinux dhcp"],
                vec!["allow", "in", "on", "waydroid0", "to", "any", "port", "53",
                     "comment", "liwinux dns"],
                vec!["route", "allow", "in", "on", "waydroid0",
                     "comment", "liwinux outbound"],
            ] {
                let st = Command::new("ufw").args(&args).stdin(Stdio::null())
                    .stdout(Stdio::null()).stderr(Stdio::null())
                    .status().await;
                if matches!(st, Ok(s) if s.success()) {
                    // Write the whole rule: a truncated report ("ufw route allow in on")
                    // hides what was done and makes it unauditable.
                    done.push(format!("ufw {}", args.join(" ")));
                }
            }
            let _ = Command::new("ufw").arg("reload").stdout(Stdio::null()).status().await;
        }

        if !d.hijack_rules.is_empty() {
            done.push(format!(
                "WARNING: {} foreign rules hijack DNS; they were NOT TOUCHED. \
                 We do not silently change another tool's configuration. \
                 Tables: {}",
                d.hijack_rules.len(),
                d.hijack_rules.iter().map(|h| h.table.as_str())
                    .collect::<Vec<_>>().join(", ")));
        }
        if done.is_empty() { done.push("nothing to do".into()); }
        Ok(done.join("\n"))
    }
}

/// Validates a logcat buffer name against a fixed list.
///
/// Accepting free text would mean the argument reaches the command line. The
/// string RETURNED from the fixed list is used; the caller-supplied input
/// itself never reaches a command.
fn valid_log_buffer(b: &str) -> Option<&'static str> {
    match b {
        "main" => Some("main"),
        "crash" => Some("crash"),
        "system" => Some("system"),
        "events" => Some("events"),
        "all" => Some("all"),
        _ => None,
    }
}

/// Clamps the line count to a sensible range.
///
/// The upper bound is mandatory: an unbounded logcat exceeds the D-Bus message
/// size and the call fails silently.
fn clamp_log_lines(n: u32) -> u32 { n.clamp(1, 2000) }

async fn run_dumpsys(argv: &[&str]) -> zbus::fdo::Result<String> {
    let mut args = vec!["--details-to-stdout", "shell", "--"];
    args.extend_from_slice(argv);
    let out = Command::new("waydroid").args(&args).stdin(Stdio::null())
        .output().await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(zbus::fdo::Error::Failed(format!(
            "waydroid shell failed (code {:?}): {}", out.status.code(), err)));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.contains("% lxc-info") && !l.trim_end().ends_with("] RUNNING"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Extracts the foreground package from `dumpsys activity activities` output.
///
/// A separate function so it can be tested against real output — Android
/// versions change this format and silently returning the wrong package loads
/// the wrong profile.
fn parse_foreground(dump: &str) -> Option<String> {
    // Try in order: the most reliable pattern first.
    for line in dump.lines() {
        let t = line.trim();
        for key in ["mResumedActivity:", "topResumedActivity=", "mFocusedActivity:"] {
            if let Some(rest) = t.split_once(key).map(|(_, r)| r) {
                if let Some(pkg) = extract_pkg(rest) { return Some(pkg); }
            }
        }
    }
    None
}

/// Extracts the package name from "... u0 com.kiloo.subwaysurf/com.sybogames...Activity t42}".
fn extract_pkg(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|tok| tok.contains('/') && tok.contains('.'))
        .and_then(|tok| tok.split('/').next())
        .map(|p| p.trim_start_matches('{').to_string())
        .filter(|p| p.contains('.') && !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{clamp_log_lines, valid_log_buffer};
    #[test]
    fn log_buffer_allowlist_rejects_injection() {
        for bad in ["main; rm -rf /", "--help", "", "MAIN", "main main",
                    "-b", "../etc/passwd"] {
            assert!(valid_log_buffer(bad).is_none(), "{bad:?} reddedilmeliydi");
        }
        for good in ["main", "crash", "system", "events", "all"] {
            assert_eq!(valid_log_buffer(good), Some(good));
        }
    }

    /// An unbounded logcat exceeds the D-Bus message limit and the call dies silently.
    #[test]
    fn log_lines_are_bounded() {
        assert_eq!(clamp_log_lines(0), 1);
        assert_eq!(clamp_log_lines(500), 500);
        assert_eq!(clamp_log_lines(u32::MAX), 2000);
    }

    use super::{extract_pkg, parse_foreground};

    const REAL: &str = "  mResumedActivity: ActivityRecord{ef108a1 u0 com.kiloo.subwaysurf/com.sybogames.chili.multidex.ChiliMultidexSupportActivity t42}";

    #[test]
    fn parses_resumed_activity() {
        assert_eq!(parse_foreground(REAL).as_deref(), Some("com.kiloo.subwaysurf"));
    }

    #[test]
    fn parses_top_resumed_variant() {
        let s = "topResumedActivity=ActivityRecord{abc u0 com.android.vending/.AssistActivity t9}";
        assert_eq!(parse_foreground(s).as_deref(), Some("com.android.vending"));
    }

    /// It must NOT INVENT a package from an unrecognised format.
    #[test]
    fn unknown_format_yields_none() {
        assert!(parse_foreground("unrelated output\nanother line").is_none());
    }

    #[test]
    fn ignores_tokens_without_package_shape() {
        assert!(extract_pkg(" u0 t42}").is_none());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("LIWD_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let conn = Connection::system().await?;
    let _srv = connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJ_PATH, Helper { conn })?
        .build()
        .await?;
    tracing::info!("liwd-helper ready — {BUS_NAME} (root, polkit protected)");

    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT"),
    }
    Ok(())
}
