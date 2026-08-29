//! Structured D-Bus errors.
//!
//! Everything used to be `zbus::fdo::Error::Failed(e.to_string())`. A UI can
//! print that, but it cannot BRANCH on it: "the helper is not installed",
//! "the session is stopped" and "there is no profile for this package" all
//! arrive as the same error with different prose. Each of those needs a
//! different button.
//!
//! The names are stable API — a client matches on them, so renaming one is a
//! breaking change. The message stays free text and is for humans only.

/// An error that crosses the bus with a name a client can match on.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "id.liwinux.Error")]
pub enum Error {
    /// Anything zbus itself produced; passed through unchanged.
    #[zbus(error)]
    ZBus(zbus::Error),

    /// The Waydroid session is not running.
    NoSession(String),

    /// `liwd-helper` is not reachable — not installed, or not started.
    ///
    /// Separate from `NoSession` on purpose: the fix is completely
    /// different (install a system service vs start a session), and the
    /// UI can offer the right one only if it can tell them apart.
    NoHelper(String),

    /// No profile exists for the requested package.
    NoProfile(String),

    /// The Waydroid window was not found (the game may not be open).
    NoWindow(String),

    /// The request is malformed: bad package name, unparsable profile.
    Invalid(String),

    /// The operation ran but failed. The last resort — prefer a specific
    /// variant whenever one fits.
    Failed(String),
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self { Error::Failed(e.to_string()) }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Failed(e.to_string()) }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are API. If one changes, every client that matches on
    /// it breaks silently — so pin them here.
    #[test]
    fn wire_names_are_stable() {
        use zbus::DBusError as _;
        for (e, want) in [
            (Error::NoSession("x".into()), "id.liwinux.Error.NoSession"),
            (Error::NoHelper("x".into()), "id.liwinux.Error.NoHelper"),
            (Error::NoProfile("x".into()), "id.liwinux.Error.NoProfile"),
            (Error::NoWindow("x".into()), "id.liwinux.Error.NoWindow"),
            (Error::Invalid("x".into()), "id.liwinux.Error.Invalid"),
            (Error::Failed("x".into()), "id.liwinux.Error.Failed"),
        ] {
            assert_eq!(e.name().as_str(), want, "{e:?}");
        }
    }

    /// The human message must survive: it is the only thing that says
    /// WHICH package or WHICH file went wrong.
    #[test]
    fn message_is_carried_through() {
        use zbus::DBusError as _;
        let e = Error::NoProfile("com.example.game".into());
        assert_eq!(e.description(), Some("com.example.game"));
    }
}
