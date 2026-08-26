//! polkit yetkilendirmesi.
//!
//! Sistem veri yolundaki her ayrıcalıklı çağrı, çağıranın kimliği polkit'e
//! sorulmadan yerine getirilmez. Çağıran özne olarak D-Bus benzersiz adını
//! kullanıyoruz (`system-bus-name`); bu, PID yarışlarına açık olan
//! PID tabanlı öznelerden güvenlidir.

use std::collections::HashMap;
use zbus::zvariant::Value;
use zbus::Connection;

#[derive(Debug, thiserror::Error)]
pub enum PolkitError {
    #[error("polkit'e ulaşılamadı: {0}")]
    Bus(#[from] zbus::Error),
    #[error("yetki reddedildi: {action}")]
    Denied { action: String },
}

/// Çağıranın `action` eylemini yapmaya yetkili olup olmadığını sorar.
///
/// `allow_interaction` true ise polkit kullanıcıya parola sorabilir.
/// Salt okunur teşhis için false, sistemi değiştiren işlemler için true uygundur.
pub async fn check(
    conn: &Connection,
    caller: &str,
    action: &str,
    allow_interaction: bool,
) -> Result<(), PolkitError> {
    let mut subject_details: HashMap<&str, Value<'_>> = HashMap::new();
    subject_details.insert("name", Value::from(caller));
    let subject = ("system-bus-name", subject_details);
    let details: HashMap<&str, &str> = HashMap::new();
    let flags: u32 = if allow_interaction { 1 } else { 0 };

    let proxy = zbus::Proxy::new(
        conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;

    // DİKKAT: polkit'in dönüş yapısı (bba{ss})'dir — details string->string'tir,
    // variant DEĞİLDİR. a{sv} beklemek "Signature mismatch" ile başarısız olur ve
    // bu, yetki reddiyle kolayca karıştırılır (bkz. PolkitError'ın ikiye ayrılması).
    let (authorized, _challenge, _info): (bool, bool, HashMap<String, String>) = proxy
        .call("CheckAuthorization", &(subject, action, details, flags, ""))
        .await?;

    if authorized {
        Ok(())
    } else {
        Err(PolkitError::Denied { action: action.to_string() })
    }
}

/// Android property anahtarı doğrulaması.
///
/// `GetProp` root olarak çalıştığı için anahtarın komut enjeksiyonuna
/// dönüşmemesi şart. Beyaz liste yaklaşımı: yalnızca gerçek property
/// adlarında görülen karakterler.
pub fn valid_prop_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::valid_prop_key;

    #[test]
    fn accepts_real_property_names() {
        for k in ["sys.boot_completed", "ro.product.cpu.abilist",
                  "ro.dalvik.vm.native.bridge", "debug.hwui.renderer"] {
            assert!(valid_prop_key(k), "reddedildi: {k}");
        }
    }

    #[test]
    fn rejects_injection_attempts() {
        for k in ["a; rm -rf /", "a`id`", "a$(id)", "a b", "a|b", "a\nb",
                  "a&b", "a>b", "'a'", "\"a\"", ""] {
            assert!(!valid_prop_key(k), "kabul edildi: {k:?}");
        }
    }

    #[test]
    fn rejects_absurdly_long_keys() {
        assert!(!valid_prop_key(&"a".repeat(65)));
    }
}
