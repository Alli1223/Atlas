//! [`Secret`]: a wrapper that makes leaking plaintext a compile error rather
//! than a code-review catch.
//!
//! `Debug` and `Display` are dead ends by construction, so `tracing::info!(?x)`
//! or a stray `format!("{x}")` cannot print one. `Serialize` fails loudly at
//! runtime rather than silently — a `Secret` reaching an API response body is a
//! bug worth a panic-shaped error, not a redacted-looking JSON field nobody
//! notices was ever wired up. `Deserialize` is unrestricted: reading a secret
//! *in* (from config, from a request body before it is ever stored) is fine —
//! only the way back out is closed off.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Plaintext secret material. See the module docs for what this does and does
/// not protect against — `Zeroize` cannot scrub a `String`'s earlier
/// reallocations, so a `Secret` should be constructed directly from the source
/// (a request body, a config value) rather than built up via `format!`/`+`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Wraps `value` as a secret.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Reveals the underlying value.
    ///
    /// Deliberately verbose and greppable: every call site is an audit point
    /// for "where does plaintext exist in memory right now".
    pub fn expose_secret(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl<T: Zeroize> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T: Zeroize> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "refusing to serialize a Secret — this is a bug, not a redaction",
        ))
    }
}

impl<'de, T: Zeroize + Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(T::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_contain_the_secret() {
        let secret = Secret::new("hunter2".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(format!("{secret}"), "[REDACTED]");
    }

    #[test]
    fn expose_secret_returns_the_real_value() {
        let secret = Secret::new("hunter2".to_owned());
        assert_eq!(secret.expose_secret(), "hunter2");
    }

    #[test]
    fn serializing_a_secret_fails_rather_than_leaking_it() {
        let secret = Secret::new("hunter2".to_owned());
        let err = serde_json::to_string(&secret).unwrap_err();
        assert!(err.to_string().contains("refusing to serialize"));
    }

    #[test]
    fn deserializing_a_secret_reads_the_plaintext_in() {
        let secret: Secret<String> = serde_json::from_str("\"hunter2\"").unwrap();
        assert_eq!(secret.expose_secret(), "hunter2");
    }
}
