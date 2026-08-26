//! `liwd-helper` istemcisi.
//!
//! Ayrıcalıklı okumalar buradan geçer. Helper yoksa çağrı başarısız olur ve
//! çağıran KENDİ kararını verir — sessizce iyimser bir değer uydurmak
//! teşhisi bozar (bkz. `Supervisor::health`).

use zbus::Connection;

const BUS: &str = "id.liwinux.Helper1";
const PATH: &str = "/id/liwinux/Helper1";

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("liwd-helper'a bağlanılamadı: {0}")]
    Connect(#[from] zbus::Error),
    #[error("liwd-helper çağrısı başarısız: {0}")]
    Call(String),
}

#[derive(Clone)]
pub struct HelperClient {
    proxy: zbus::Proxy<'static>,
}

impl HelperClient {
    /// Helper'a bağlanır. Servis çalışmıyorsa `Err` döner.
    pub async fn connect() -> Result<Self, HelperError> {
        let conn = Connection::system().await?;
        let proxy = zbus::Proxy::new(&conn, BUS, PATH, BUS).await?;
        // Gerçekten orada mı: introspect ucuz ve yetkilendirme gerektirmez.
        // (fdo::Error ayrı bir tür; Call'a sarıyoruz.)
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

    /// Ön plandaki paket adı. Boş dize = tespit edilemedi.
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

    pub async fn net_diagnose(&self) -> Result<String, HelperError> {
        self.proxy.call("NetDiagnose", &())
            .await.map_err(|e| HelperError::Call(e.to_string()))
    }
}
