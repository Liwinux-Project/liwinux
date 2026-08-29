//! Managing the Waydroid window through KWin.
//!
//! Why it is needed: touches travel in SCREEN space. If the window is not
//! aligned with the output, profile coordinates shift and edge touches fall
//! outside it. Measured: at 10,10 / 2540x1370 both x=0.0 and x=1.0 were lost.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

const SCRIPT_NAME: &str = "liwinux-fullscreen";
const ACTIVATE_SCRIPT: &str = "liwinux-activate";
const REPORT_SCRIPT: &str = "liwinux-report";

/// Window geometry as reported by the KWin script.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct WindowGeometry {
    pub found: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub fullscreen: bool,
}

#[derive(Default)]
pub struct WindowState {
    geometry: RwLock<WindowGeometry>,
    /// Whether fullscreen was attempted once in this session. Reset when the
    /// session goes down.
    attempted: RwLock<bool>,
}

impl WindowState {
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }

    pub async fn set(&self, g: WindowGeometry) {
        *self.geometry.write().await = g;
    }

    pub async fn get(&self) -> WindowGeometry {
        *self.geometry.read().await
    }

    pub async fn fullscreen_attempted(&self) -> bool { *self.attempted.read().await }
    pub async fn mark_fullscreen_attempted(&self) { *self.attempted.write().await = true; }

    /// Called when the session goes down, so a new session retries.
    pub async fn reset(&self) {
        *self.attempted.write().await = false;
        *self.geometry.write().await = WindowGeometry::default();
    }

    /// Clears the fullscreen flag if the window disappeared.
    ///
    /// Waiting for the session to stop was NOT ENOUGH: users keep the session
    /// up and close/reopen the `show-full-ui` window. In that case the flag was
    /// never cleared and a new window was never made fullscreen — a single
    /// attempt across a 10-hour daemon lifetime. This actually happened.
    ///
    /// If the window IS there but not fullscreen, nothing is touched: the user
    /// may have left it deliberately and forcing it back would be hostile.
    ///
    /// Returns `true` when a retry is now allowed for a new window.
    pub async fn note_window_gone(&self) -> bool {
        // found == false -> no window. If it is there, do nothing.
        if self.geometry.read().await.found { return false; }
        std::mem::replace(&mut *self.attempted.write().await, false)
    }
}

/// Runs the fullscreen script once.
///
/// The result comes back via `ReportWindowGeometry`; this function only
/// triggers it. The KWin scripting API does not return output to the caller.
pub async fn request_fullscreen() -> Result<()> {
    let path = script_path().context("fullscreen.js not found")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    // Loading the same name twice runs it twice; unload first.
    let _: Result<bool, _> = p.call("unloadScript", &(SCRIPT_NAME,)).await;
    let _id: i32 = p.call("loadScript", &(path.to_string_lossy().as_ref(), SCRIPT_NAME))
        .await.context("loadScript failed")?;
    let _: () = p.call("start", &()).await.context("could not start script")?;
    Ok(())
}

/// Raises and focuses the Waydroid window.
///
/// Mandatory before taking a screenshot: `spectacle -a` captures the ACTIVE
/// window, and if that is the terminal you get a picture of the terminal.
pub async fn activate() -> Result<()> {
    let path = script_path_named("activate.js").context("activate.js not found")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: Result<bool, _> = p.call("unloadScript", &(ACTIVATE_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript",
        &(path.to_string_lossy().as_ref(), ACTIVATE_SCRIPT)).await?;
    let _: () = p.call("start", &()).await?;
    Ok(())
}

/// Queries the window state — CHANGES NOTHING.
///
/// The result comes back via `ReportWindowGeometry`. Calling
/// `request_fullscreen` just to learn the state would force fullscreen back
/// on a user who deliberately left it.
pub async fn request_report() -> Result<()> {
    let path = script_path_named("report.js").context("report.js not found")?;
    let conn = zbus::Connection::session().await?;
    let p = zbus::Proxy::new(&conn, "org.kde.KWin", "/Scripting",
                             "org.kde.kwin.Scripting").await?;
    let _: Result<bool, _> = p.call("unloadScript", &(REPORT_SCRIPT,)).await;
    let _id: i32 = p.call("loadScript",
        &(path.to_string_lossy().as_ref(), REPORT_SCRIPT)).await?;
    let _: () = p.call("start", &()).await?;
    Ok(())
}

/// Retries a few times until the window appears.
///
/// When boot completes the window may NOT EXIST YET: `show-full-ui` is a
/// separate step and the compositor takes time to create the surface. A single
/// attempt usually arrives too early.
pub async fn fullscreen_with_retry(
    state: Arc<WindowState>,
    attempts: u32,
    gap: std::time::Duration,
) -> bool {
    for i in 1..=attempts {
        if let Err(e) = request_fullscreen().await {
            tracing::warn!(attempt = i, error = %e, "could not send fullscreen request");
        }
        tokio::time::sleep(gap).await;
        let g = state.get().await;
        if g.found && g.fullscreen {
            tracing::info!(
                width = g.width, height = g.height,
                "Waydroid window set to fullscreen");
            return true;
        }
    }
    let g = state.get().await;
    if g.found {
        tracing::warn!(
            width = g.width, height = g.height, x = g.x, y = g.y,
            "window found but could not be made fullscreen — touch coordinates may shift");
    } else {
        tracing::info!("no Waydroid window — fullscreen skipped");
    }
    false
}

/// Looks for `fullscreen.js`. The WORKING DIRECTORY IS NOT CONSULTED (the same
/// lesson as the profile store: cwd-dependent behaviour produces bugs nobody
/// can diagnose).
fn script_path() -> Option<std::path::PathBuf> { script_path_named("fullscreen.js") }

fn script_path_named(name: &str) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".local").join("share")))
    {
        candidates.push(data.join("liwinux").join("kwin").join(name));
    }
    candidates.push(format!("/usr/share/liwinux/kwin/{name}").into());
    candidates.push(format!("/usr/local/share/liwinux/kwin/{name}").into());
    for c in candidates {
        if c.is_file() { return Some(c); }
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    for _ in 0..4 {
        let cand = dir.join("scripts").join("kwin").join(name);
        if cand.is_file() { return Some(cand); }
        dir = dir.parent()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fullscreen must be RETRIED when the session restarts.
    #[tokio::test]
    async fn reset_clears_attempt_flag() {
        let s = WindowState::new();
        s.mark_fullscreen_attempted().await;
        assert!(s.fullscreen_attempted().await);
        s.reset().await;
        assert!(!s.fullscreen_attempted().await);
        assert!(!s.get().await.found);
    }

    #[tokio::test]
    async fn geometry_starts_empty_and_updates() {
        let s = WindowState::new();
        assert!(!s.get().await.found);
        s.set(WindowGeometry {
            found: true, x: 0, y: 0, width: 2560, height: 1440, fullscreen: true,
        }).await;
        let g = s.get().await;
        assert!(g.found && g.fullscreen);
        assert_eq!((g.width, g.height), (2560, 1440));
    }

    /// Closing the window must re-enable the fullscreen attempt.
    ///
    /// This actually happened: the user kept the session up and closed/reopened
    /// the `show-full-ui` window; because the flag was never cleared, no new
    /// window was made fullscreen for 10 hours.
    #[tokio::test]
    async fn closed_window_reenables_fullscreen_attempt() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, x: 0, y: 0,
            width: 2560, height: 1440, fullscreen: true }).await;
        s.mark_fullscreen_attempted().await;

        // Nothing must happen while the window is there.
        assert!(!s.note_window_gone().await, "must not reset while the window exists");
        assert!(s.fullscreen_attempted().await);

        // The window is gone.
        s.set(WindowGeometry::default()).await;
        assert!(s.note_window_gone().await, "kapanma bildirilmeli");
        assert!(!s.fullscreen_attempted().await, "the flag must be cleared");
    }

    /// If the user left fullscreen DELIBERATELY it must not be forced back.
    #[tokio::test]
    async fn user_leaving_fullscreen_is_not_fought() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, x: 100, y: 100,
            width: 1280, height: 720, fullscreen: false }).await;
        s.mark_fullscreen_attempted().await;
        assert!(!s.note_window_gone().await);
        assert!(s.fullscreen_attempted().await, "the user decision must be kept");
    }

    /// With no window ever seen and no attempt made there is nothing to report.
    #[tokio::test]
    async fn nothing_to_report_when_never_attempted() {
        let s = WindowState::new();
        assert!(!s.note_window_gone().await);
    }

    /// The same closure must not be reported twice — otherwise every poll
    /// forces fullscreen and fights the user.
    #[tokio::test]
    async fn closure_is_reported_once() {
        let s = WindowState::new();
        s.set(WindowGeometry { found: true, ..Default::default() }).await;
        s.mark_fullscreen_attempted().await;
        s.set(WindowGeometry::default()).await;
        assert!(s.note_window_gone().await);
        assert!(!s.note_window_gone().await, "ikinci kez bildirilmemeli");
    }
}
