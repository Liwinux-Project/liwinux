//! polkit authorization.
//!
//! No privileged call on the system bus is served without asking polkit about
//! the caller's identity. The caller subject is the D-Bus unique name
//! (`system-bus-name`), which is safer than PID-based subjects — those are
//! open to PID-reuse races.

use std::collections::HashMap;
use zbus::zvariant::Value;
use zbus::Connection;

#[derive(Debug, thiserror::Error)]
pub enum PolkitError {
    #[error("could not reach polkit: {0}")]
    Bus(#[from] zbus::Error),
    #[error("authorization denied: {action}")]
    Denied { action: String },
}

/// Asks whether the caller is authorized to perform `action`.
///
/// With `allow_interaction` true, polkit may prompt the user for a password.
/// Use false for read-only diagnostics, true for operations that change the
/// system.
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

    // CAREFUL: polkit's return signature is (bba{ss}) — details is
    // string->string, NOT variant. Expecting a{sv} fails with "Signature
    // mismatch", which is easily mistaken for an authorization denial (hence
    // PolkitError being split into two variants).
    let (authorized, _challenge, _info): (bool, bool, HashMap<String, String>) = proxy
        .call("CheckAuthorization", &(subject, action, details, flags, ""))
        .await?;

    if authorized {
        Ok(())
    } else {
        Err(PolkitError::Denied { action: action.to_string() })
    }
}

/// Validates an Android property key.
///
/// `GetProp` runs as root, so the key must not be able to turn into command
/// injection. Allowlist approach: only characters that appear in genuine
/// property names.
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
            assert!(valid_prop_key(k), "rejected: {k}");
        }
    }

    #[test]
    fn rejects_injection_attempts() {
        for k in ["a; rm -rf /", "a`id`", "a$(id)", "a b", "a|b", "a\nb",
                  "a&b", "a>b", "'a'", "\"a\"", ""] {
            assert!(!valid_prop_key(k), "accepted: {k:?}");
        }
    }

    #[test]
    fn rejects_absurdly_long_keys() {
        assert!(!valid_prop_key(&"a".repeat(65)));
    }
}
