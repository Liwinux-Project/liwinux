//! `liwd-helper` client.
//!
//! Privileged reads go through here. If the helper is absent the call fails
//! and the caller makes its OWN decision — silently inventing an optimistic
//! value corrupts diagnosis (see `Supervisor::health`).

use zbus::Connection;

const BUS: &str = "id.liwinux.Helper1";
const PATH: &str = "/id/liwinux/Helper1";

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("could not connect to liwd-helper: {0}")]
    Connect(#[from] zbus::Error),
    #[error("liwd-helper call failed: {0}")]
    Call(String),
}

#[derive(Clone)]
pub struct HelperClient {
    proxy: zbus::Proxy<'static>,
}

impl HelperClient {
    /// Connects to the helper. Returns `Err` if the service is not running.
    pub async fn connect() -> Result<Self, HelperError> {
        let conn = Connection::system().await?;
        let proxy = zbus::Proxy::new(&conn, BUS, PATH, BUS).await?;
        // Is it really there? Introspection is cheap and needs no authorization.
        // (fdo::Error is a separate type; wrap it into Call.)
        proxy.introspect().await.map_err(|e| HelperError::Call(e.to_string()))?;
        Ok(Self { proxy })
    }

    pub async fn boot_completed(&self) -> Result<bool, HelperError> {
        self.proxy.call("BootCompleted", &())
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn get_prop(&self, key: &str) -> Result<String, HelperError> {
        self.proxy.call("GetProp", &(key,))
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    /// Foreground package name. Empty string = could not be determined.
    pub async fn foreground_package(&self) -> Result<String, HelperError> {
        self.proxy.call("ForegroundPackage", &())
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn surface_layers(&self) -> Result<String, HelperError> {
        self.proxy.call("SurfaceLayers", &())
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn surface_latency(&self, layer: &str) -> Result<String, HelperError> {
        self.proxy.call("SurfaceLatency", &(layer,))
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn set_pointer_location(&self, enabled: bool) -> Result<(), HelperError> {
        self.proxy.call("SetPointerLocation", &(enabled,))
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    /// Requests a WRITE handle to Waydroid's touch pipe.
    ///
    /// On success the keymapper bypasses the compositor chain entirely and
    /// coordinates are not clamped — the prerequisite for unbounded aim
    /// (`docs/mouse-aim.md`). The returned size is the ANDROID display, not
    /// the host window.
    ///
    /// Failure is NOT fatal: the caller can fall back to the uinput path. But
    /// it must not fall back silently — the user needs to know which path is
    /// active, because how aim feels depends entirely on it.
    pub async fn open_touch_pipe(&self)
        -> Result<(std::fs::File, u32, u32), HelperError>
    {
        let (fd, w, h): (zbus::zvariant::OwnedFd, u32, u32) =
            self.proxy.call("OpenTouchPipe", &())
                .await.map_err(|e| HelperError::Call(e.to_string()))?;
        Ok((std::fs::File::from(std::os::fd::OwnedFd::from(fd)), w, h))
    }

    /// logcat with monotonic timestamps (for diagnostic correlation).
    ///
    /// An older helper does not have this method; the caller must fall back to
    /// `Logcat`. Returning empty silently would look like "no events at all"
    /// and send diagnosis completely the wrong way.
    pub async fn log_trace(&self, buffer: &str, lines: u32)
        -> Result<String, HelperError>
    {
        self.proxy.call("LogTrace", &(buffer, lines))
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn logcat(&self, buffer: &str, lines: u32)
        -> Result<String, HelperError>
    {
        self.proxy.call("Logcat", &(buffer, lines))
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }

    pub async fn net_diagnose(&self) -> Result<String, HelperError> {
        self.proxy.call("NetDiagnose", &())
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }
}
