//! Dokunuş enjeksiyon arka uçları.
//!
//! Motor `TouchAction` üretir; arka uç onu Android'e ulaştırır. Arka uç
//! takılabilir çünkü hangi yolun çalışacağı ölçümle belirlenecek:
//!
//! * `uinput`  — host'ta sanal dokunmatik ekran; libinput → KWin → wl_touch
//!               → Waydroid. Konteyner değişikliği ve Java gerektirmez.
//! * `debug`   — sadece yazdırır; eşlemeyi Android'e dokunmadan doğrulamak için.
//! * (ileride) `android_socket` — app_process sunucusu + injectInputEvent().

use super::touch::TouchAction;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("arka uç başlatılamadı: {0}")]
    Init(String),
    #[error("dokunuş gönderilemedi: {0}")]
    Dispatch(String),
}

pub trait TouchBackend: Send {
    /// Eylemleri sırayla uygular. Tek bir çağrıda gelen eylemler AYNI
    /// kareye ait sayılır ve tek SYN_REPORT ile bitirilmelidir; aksi halde
    /// çoklu dokunuş "ayrı ayrı parmaklar" gibi görünür.
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError>;

    /// Tüm parmakları kaldırır (acil durdurma, profil değişimi).
    fn release_all(&mut self) -> Result<(), BackendError>;

    fn name(&self) -> &'static str;
}

/// Hiçbir yere enjekte etmeyen, sadece kaydeden arka uç.
#[derive(Debug, Default)]
pub struct DebugBackend {
    pub log: Vec<TouchAction>,
}

impl TouchBackend for DebugBackend {
    fn dispatch(&mut self, actions: &[TouchAction]) -> Result<(), BackendError> {
        self.log.extend_from_slice(actions);
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), BackendError> { Ok(()) }
    fn name(&self) -> &'static str { "debug" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::touch::Norm;

    #[test]
    fn debug_backend_records_in_order() {
        let mut b = DebugBackend::default();
        b.dispatch(&[
            TouchAction::Down { id: 0, at: Norm::new(0.1, 0.2) },
            TouchAction::Move { id: 0, at: Norm::new(0.3, 0.4) },
        ]).unwrap();
        b.dispatch(&[TouchAction::Up { id: 0 }]).unwrap();
        assert_eq!(b.log.len(), 3);
        assert!(matches!(b.log[2], TouchAction::Up { id: 0 }));
    }
}
