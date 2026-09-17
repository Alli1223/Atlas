//! GitHub PAT validation: the cheap, side-effect-free probe Phase 11 asks for,
//! specialised now because GitHub is the vault's only Phase-12 consumer.
//!
//! # Scope and expiry discovery
//!
//! `GET /user` with the token doubles as the validation probe (a PAT that
//! cannot fetch the authenticated user is not valid, full stop) and as the
//! source of two response headers:
//!
//! - `x-oauth-scopes` — comma-separated scopes. Absent for fine-grained PATs,
//!   which do not use OAuth scopes at all; `None` there is correct, not missing
//!   data.
//! - `github-authentication-token-expiration` — parsed in **both** layouts
//!   GitHub has been observed to send: `2006-01-02 15:04:05 MST` and
//!   `2006-01-02 15:04:05 -0700` (`docs/research/corrections.md` #3). A missing
//!   header means "no expiry information supplied", which is deliberately
//!   `None` rather than treated as "never expires" (`corrections.md` #5) —
//!   those are different claims and only GitHub can make the second one.

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::vault::credential::{CredentialStatus, ValidationResult};
use crate::vault::secret::Secret;

const GITHUB_USER_ENDPOINT: &str = "https://api.github.com/user";

/// Probes a GitHub PAT against `GET /user` and reads its scope/expiry headers.
///
/// Never returns an `Err` for a merely-invalid token — that is a normal
/// [`CredentialStatus::Invalid`] result. `Err` is reserved for the probe
/// itself failing (network error, unparseable response), which the caller
/// should surface distinctly from "the token is bad".
pub async fn validate(token: &Secret<String>) -> anyhow::Result<ValidationResult> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("atlas/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let response = client
        .get(GITHUB_USER_ENDPOINT)
        .bearer_auth(token.expose_secret())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !response.status().is_success() {
        return Ok(ValidationResult {
            status: CredentialStatus::Invalid,
            expires_at: None,
            scopes: None,
        });
    }

    let scopes = response
        .headers()
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());

    let expires_at = response
        .headers()
        .get("github-authentication-token-expiration")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_github_expiration);

    let status = match expires_at {
        Some(expiry) if expiry <= Utc::now() => CredentialStatus::Expired,
        _ => CredentialStatus::Valid,
    };

    Ok(ValidationResult {
        status,
        expires_at,
        scopes,
    })
}

/// Parses `github-authentication-token-expiration`'s two known layouts.
///
/// The numeric-offset layout is tried first and is exact. The named-zone one
/// is naive — chrono has no `%Z` parser, since a zone abbreviation like `MST`
/// is not a fixed offset it could compute from — so that fallback treats the
/// clock time as UTC outright. GitHub's expiry granularity is a day, and being
/// off by a timezone's worth of hours in that fallback, on a value rendered to
/// the user as "expires in N days", is not a correctness issue worth chasing.
///
/// The order matters: `"2026-12-01 00:00:00 -0700"` also matches the
/// named-zone shape if tried first (`NaiveDateTime` happily parses the part
/// before the last space and silently drops the real offset), so the exact
/// layout must be tried before the lossy one, not after.
fn parse_github_expiration(value: &str) -> Option<DateTime<Utc>> {
    // Layout 1: "2006-01-02 15:04:05 -0700" — a real, computable offset.
    if let Ok(fixed) = DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S %z") {
        return Some(fixed.with_timezone(&Utc));
    }

    // Layout 2: "2006-01-02 15:04:05 MST" — strip the trailing zone name and
    // interpret the clock time as UTC; see the caveat above.
    if let Some((datetime_part, _zone)) = value.rsplit_once(' ')
        && let Ok(naive) = NaiveDateTime::parse_from_str(datetime_part, "%Y-%m-%d %H:%M:%S")
    {
        return Some(naive.and_utc());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_named_zone_layout() {
        let parsed = parse_github_expiration("2026-12-01 00:00:00 UTC").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-12-01T00:00:00+00:00");
    }

    #[test]
    fn parses_the_numeric_offset_layout() {
        let parsed = parse_github_expiration("2026-12-01 00:00:00 -0700").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-12-01T07:00:00+00:00");
    }

    #[test]
    fn an_unparseable_value_is_none_not_an_error() {
        assert!(parse_github_expiration("not a date").is_none());
    }
}
