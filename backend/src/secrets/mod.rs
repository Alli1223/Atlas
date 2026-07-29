//! The encrypted secrets vault: the GitHub PAT and the Claude/Gemini API keys,
//! encrypted at rest and never returned over the wire.
//!
//! # The three ways a secret is kept from leaking
//!
//! This is the most security-critical module in Atlas — a leak here is every
//! integration credential the instance holds — so the defences are layered and
//! each is designed to fail *loud* rather than *open*:
//!
//! 1. **At rest**: every secret is sealed with XChaCha20-Poly1305 under a key
//!    derived from the master key ([`crypto`]). The database file alone is
//!    useless without `ATLAS_MASTER_KEY`.
//! 2. **In the process**: the plaintext only ever exists inside a [`Secret<T>`],
//!    whose `Debug` and `Display` are redacted, which has **no `Serialize` impl at
//!    all**, and which zeroes its buffer on drop. Putting a secret in a log line
//!    or a JSON response is therefore a *compile error*, not something a reviewer
//!    has to catch.
//! 3. **On the wire**: the API models the ciphertext and the plaintext in types
//!    that cannot be serialised, and the response DTO ([`CredentialDto`]) is a
//!    separate struct that has no field for either — see [`crate::api::credentials`].
//!
//! # Layout
//!
//! - [`crypto`] — the AEAD primitive and HKDF key derivation.
//! - [`vault`] — [`vault::Vault`], which ties the cipher to the row it protects
//!   via AAD binding, plus the [`vault::Validator`] trait the provider agents
//!   (GitHub, Gemini) implement to probe a credential.
//! - this module — the [`Secret<T>`] wrapper, the [`Provider`]/[`CredentialStatus`]
//!   enums, the `api_credentials` row and its queries.
//!
//! ## Database access
//!
//! The runtime `sqlx::query_as::<_, T>("...")` API throughout, every SQL string a
//! `&'static str`, exactly as [`crate::domain`] and [`crate::auth`] do — so the
//! absence of `AssertSqlSafe` here is the same real signal it is there: no SQL is
//! built by formatting.

pub mod crypto;
pub mod vault;

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::AppResult;
use crate::secrets::crypto::Sealed;

pub use crypto::{Crypto, KEY_VERSION};
pub use vault::{ValidationOutcome, Validator, Vault};

/// How many characters of a secret are kept in cleartext for display.
///
/// The last four only — enough to tell two tokens apart in the UI, few enough to
/// reveal nothing usable. A four-character suffix of a 40-character token is not a
/// meaningful head start for a guesser.
pub const LAST_FOUR_LEN: usize = 4;

/// How close to `expires_at` a still-valid credential is reported as *expiring*.
///
/// The frontend renders a warning pill inside this window (`TODO.md` Phase 11:
/// "expiring in N days"). Fourteen days is enough notice to rotate a PAT before
/// an integration breaks.
const EXPIRING_WINDOW_DAYS: i64 = 14;

// ---------------------------------------------------------------------------
// Secret<T>: leaking is a compile error, not a review catch.
// ---------------------------------------------------------------------------

/// A value whose plaintext must never reach a log, a `Debug` dump, or a response.
///
/// The whole point is that the *only* way to read the inner value is the
/// deliberately-named [`Secret::expose`], which shows up at every audit point when
/// someone greps for it. Every other route out is closed:
///
/// - [`fmt::Debug`] and [`fmt::Display`] render `[REDACTED]`, so
///   `tracing::info!(?secret)` and `format!("{secret}")` are both dead ends;
/// - there is **no `Serialize` impl**, so a struct that tries to put a `Secret`
///   in a JSON response *does not compile* — the strongest possible guarantee,
///   and the reason this is preferred over a `Serialize` that errors at runtime;
/// - the buffer is zeroed on drop, so a freed plaintext does not linger in the
///   heap to be scraped from a core dump.
///
/// This is the fuller form of [`crate::config::SecretString`], which the config
/// loader needed before this module existed; that type guards the master key with
/// the same redaction, and Phase 11 does not disturb it.
///
/// # Caveat
///
/// `Zeroize` on `String`/`Vec` cannot scrub a buffer the value already outgrew
/// and reallocated away from. Construct a secret directly into the wrapper — which
/// [`crate::secrets::crypto::Crypto::open`] does — rather than building it up and
/// wrapping it late.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Wraps a value as a secret.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Reveals the protected value.
    ///
    /// Deliberately verbose and greppable: every call site is a place a secret
    /// crosses back into cleartext, and every one should be visible in review.
    pub fn expose(&self) -> &T {
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

// Inbound is fine: a request body carrying a new secret deserialises straight
// into the wrapper, so the plaintext is protected from the moment it exists and
// is zeroed when the request struct drops. There is deliberately **no matching
// `impl Serialize`** — its absence is the guarantee that a DTO field of type
// `Secret<_>` fails `#[derive(Serialize)]` at compile time. Do not add one, not
// even one that errors: that would trade a compile error for a runtime one.
impl<'de, T: Zeroize + Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(T::deserialize(deserializer)?))
    }
}

/// The last [`LAST_FOUR_LEN`] characters of a secret, for display.
///
/// Character-wise, not byte-wise, so a multi-byte secret cannot panic here — even
/// though real tokens are ASCII.
pub fn last_four(secret: &str) -> String {
    let count = secret.chars().count();
    secret
        .chars()
        .skip(count.saturating_sub(LAST_FOUR_LEN))
        .collect()
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Why a provider string could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown credential provider {0:?}: expected one of github, anthropic, gemini, smtp")]
pub struct ProviderError(String);

/// Which integration a credential is for.
///
/// A closed set, pinned by a `CHECK` in migration 0009 *and* by this enum's
/// `Decode`: a row with a provider outside the four is corrupt, and defaulting it
/// would hide that. The same two-guard shape as [`crate::domain::StatusCategory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// A GitHub personal access token (Phase 12).
    Github,
    /// An Anthropic / Claude API key (Phase 13).
    Anthropic,
    /// A Google Gemini API key (Phase 14).
    Gemini,
    /// SMTP credentials for outbound email (Phase 17).
    Smtp,
}

impl Provider {
    /// The provider's database and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Smtp => "smtp",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provider {
    type Err = ProviderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(Self::Github),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            "smtp" => Ok(Self::Smtp),
            other => Err(ProviderError(other.to_owned())),
        }
    }
}

impl Type<Sqlite> for Provider {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Provider {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Provider {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

// ---------------------------------------------------------------------------
// CredentialStatus (stored) and PillStatus (effective, for the UI)
// ---------------------------------------------------------------------------

/// Why a status string could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown credential status {0:?}")]
pub struct CredentialStatusError(String);

/// The **stored** validation state of a credential.
///
/// Four values, pinned by a `CHECK` in 0009. This is what the last validation
/// probe concluded (or `Unchecked` if none has run). It deliberately does *not*
/// include "expiring" — that is not a fact a probe returns, it is a function of
/// `expires_at` and the clock, computed for display by [`PillStatus::effective`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialStatus {
    /// Never validated: stored, but not yet probed against its provider.
    Unchecked,
    /// The last probe succeeded.
    Valid,
    /// The last probe rejected it (revoked, wrong scopes, malformed).
    Invalid,
    /// The provider reported it as expired.
    Expired,
}

impl CredentialStatus {
    /// The status's database spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
        }
    }
}

impl fmt::Display for CredentialStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CredentialStatus {
    type Err = CredentialStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unchecked" => Ok(Self::Unchecked),
            "valid" => Ok(Self::Valid),
            "invalid" => Ok(Self::Invalid),
            "expired" => Ok(Self::Expired),
            other => Err(CredentialStatusError(other.to_owned())),
        }
    }
}

impl Type<Sqlite> for CredentialStatus {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for CredentialStatus {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CredentialStatus {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// The status the frontend renders as a pill, derived from the stored status plus
/// `expires_at` and the clock.
///
/// The extra state over [`CredentialStatus`] is `Expiring`: a still-valid
/// credential inside [`EXPIRING_WINDOW_DAYS`] of its expiry. Computing it here
/// rather than storing it means it is always correct against *now*, with no
/// scheduled job needed to flip a stored flag as the deadline passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PillStatus {
    /// Stored but never probed.
    Unchecked,
    /// Valid, and not near expiry.
    Valid,
    /// Valid, but within the warning window of `expires_at`.
    Expiring,
    /// Past `expires_at`, or the provider reported it expired.
    Expired,
    /// The last probe rejected it.
    Invalid,
}

impl PillStatus {
    /// Resolves the effective status for display.
    ///
    /// Precedence, most-severe first:
    /// 1. an `expires_at` already in the past reads as `Expired`, whatever the
    ///    stored status says — the clock is authoritative once the deadline has
    ///    passed, even if no re-probe has run;
    /// 2. a stored `Invalid`/`Expired` is reported as-is;
    /// 3. a stored `Valid` inside the warning window is `Expiring`;
    /// 4. otherwise the stored status maps straight across.
    pub fn effective(
        stored: CredentialStatus,
        expires_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        if let Some(expiry) = expires_at
            && expiry <= now
        {
            return Self::Expired;
        }

        match stored {
            CredentialStatus::Invalid => Self::Invalid,
            CredentialStatus::Expired => Self::Expired,
            CredentialStatus::Unchecked => Self::Unchecked,
            CredentialStatus::Valid => match expires_at {
                Some(expiry) if expiry - now <= Duration::days(EXPIRING_WINDOW_DAYS) => {
                    Self::Expiring
                }
                _ => Self::Valid,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The api_credentials row, and the metadata DTO
// ---------------------------------------------------------------------------

/// A row of `api_credentials`, exactly as stored — ciphertext included.
///
/// **Not `Serialize`, and it never will be.** It carries `ciphertext` and
/// `nonce`, and the only way onto the wire is [`CredentialDto`], which has no
/// field for either. A row is a row; a DTO is what the client sees. See
/// [`crate::domain::project::Project`] for the same convention where nothing is
/// yet secret — here it is load-bearing.
#[derive(Debug, Clone, FromRow)]
pub struct Credential {
    /// UUID v7, as text. Also the AAD identity the ciphertext is bound to.
    pub id: String,
    /// Which integration this credential is for.
    pub provider: Provider,
    /// A human label, unique within the provider.
    pub label: String,
    /// The sealed secret. Never leaves this struct except through the vault.
    pub ciphertext: Vec<u8>,
    /// The nonce the secret was sealed under.
    pub nonce: Vec<u8>,
    /// The `KEY_VERSION` that sealed it.
    pub key_version: i64,
    /// The last few characters of the secret, in cleartext, for display.
    pub last_four: String,
    /// What the last validation probe concluded.
    pub status: CredentialStatus,
    /// When the last probe ran, if one has.
    pub last_validated_at: Option<DateTime<Utc>>,
    /// When the credential expires, if the provider told us.
    pub expires_at: Option<DateTime<Utc>>,
    /// The provider scopes, as a JSON array, if discovered.
    pub scopes: Option<String>,
    /// The admin who stored it. `NULL` if that account was later removed.
    pub created_by: Option<String>,
    /// When it was stored.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// A credential as the API describes it: **metadata only, never the secret.**
///
/// A separate struct from [`Credential`] on purpose — this is the one that
/// derives `Serialize`, and it structurally cannot carry the ciphertext, the
/// nonce, or the plaintext, because it has no field for them. The secret cannot
/// leak through this type by any edit short of adding a field and a decrypt call,
/// both of which are visible in review.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialDto {
    /// UUID v7, as text.
    pub id: String,
    /// Which integration this is for.
    pub provider: Provider,
    /// The human label.
    pub label: String,
    /// The last four characters of the secret — all the UI ever sees of it.
    #[schema(example = "a1b2")]
    pub last_four: String,
    /// The effective status pill, resolved against the current time.
    pub status: PillStatus,
    /// When the credential expires, if known.
    pub expires_at: Option<DateTime<Utc>>,
    /// The provider scopes, if discovered.
    pub scopes: Vec<String>,
    /// When it was last validated, if ever.
    pub last_validated_at: Option<DateTime<Utc>>,
    /// When it was stored.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl CredentialDto {
    /// Builds the DTO from a row, resolving the effective status against `now`.
    pub fn from_row(credential: &Credential, now: DateTime<Utc>) -> Self {
        Self {
            id: credential.id.clone(),
            provider: credential.provider,
            label: credential.label.clone(),
            last_four: credential.last_four.clone(),
            status: PillStatus::effective(credential.status, credential.expires_at, now),
            expires_at: credential.expires_at,
            scopes: parse_scopes(credential.scopes.as_deref()),
            last_validated_at: credential.last_validated_at,
            created_at: credential.created_at,
            updated_at: credential.updated_at,
        }
    }
}

/// Parses the stored scopes JSON into a list, tolerating absence and malformed
/// data — a garbled scopes column must not turn a credential list into a 500.
fn parse_scopes(scopes: Option<&str>) -> Vec<String> {
    scopes
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .unwrap_or_default()
}

/// A new credential ready to insert: the sealed secret plus its display metadata.
#[derive(Debug)]
pub struct NewCredential {
    /// Which integration it is for.
    pub provider: Provider,
    /// The human label.
    pub label: String,
    /// The sealed secret, nonce, and key version.
    pub sealed: Sealed,
    /// The last four characters of the plaintext, precomputed by the caller.
    pub last_four: String,
    /// The admin storing it.
    pub created_by: String,
}

/// Every column of `api_credentials`. A macro so `concat!` can splice it into a
/// `&'static str` — the same trick [`crate::domain::project`] uses.
macro_rules! credential_columns {
    () => {
        "id, provider, label, ciphertext, nonce, key_version, last_four, status, \
         last_validated_at, expires_at, scopes, created_by, created_at, updated_at"
    };
}

/// Inserts a credential and returns the stored row.
///
/// The `id` is generated here so the caller can bind the ciphertext's AAD to it
/// *before* the insert — see [`vault::Vault::seal_for`].
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    new: &NewCredential,
    now: DateTime<Utc>,
) -> AppResult<Credential> {
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO api_credentials \
           (id, provider, label, ciphertext, nonce, key_version, last_four, status, \
            last_validated_at, expires_at, scopes, created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'unchecked', NULL, NULL, NULL, ?, ?, ?)",
    )
    .bind(id)
    .bind(new.provider)
    .bind(&new.label)
    .bind(&new.sealed.ciphertext)
    .bind(&new.sealed.nonce)
    .bind(new.sealed.key_version)
    .bind(&new.last_four)
    .bind(&new.created_by)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, id).await?.ok_or_else(|| {
        crate::error::AppError::internal(anyhow::anyhow!("the credential just inserted is missing"))
    })
}

/// Generates a fresh credential id.
///
/// Exposed so the API layer can mint the id, bind the ciphertext to it, and pass
/// the same id to [`insert`] — the AAD and the primary key must be the same value.
pub fn new_id() -> String {
    Uuid::now_v7().to_string()
}

/// Every credential, ordered for display. Metadata and ciphertext both — callers
/// that only need metadata build [`CredentialDto`] from each.
pub async fn list(db: &Db) -> AppResult<Vec<Credential>> {
    Ok(sqlx::query_as::<_, Credential>(concat!(
        "SELECT ",
        credential_columns!(),
        " FROM api_credentials ORDER BY provider, label"
    ))
    .fetch_all(db.reader())
    .await?)
}

/// Finds a credential by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Credential>> {
    Ok(sqlx::query_as::<_, Credential>(concat!(
        "SELECT ",
        credential_columns!(),
        " FROM api_credentials WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a credential by id inside an open transaction.
pub async fn find_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Credential>> {
    Ok(sqlx::query_as::<_, Credential>(concat!(
        "SELECT ",
        credential_columns!(),
        " FROM api_credentials WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Whether a `(provider, label)` pair is already taken, case-insensitively.
///
/// Checked inside the write transaction so it cannot race the insert; the
/// `UNIQUE` index in 0009 is the real guarantee, and this turns its 500 into a
/// 409 that says what to fix.
pub async fn label_taken(
    tx: &mut sqlx::SqliteConnection,
    provider: Provider,
    label: &str,
) -> AppResult<bool> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM api_credentials WHERE provider = ? AND label = ?)",
    )
    .bind(provider)
    .bind(label)
    .fetch_one(&mut *tx)
    .await?;
    Ok(taken)
}

/// Deletes a credential, returning whether a row was removed.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM api_credentials WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Records the outcome of a validation probe.
///
/// Writes the new status, the discovered scopes and expiry, and stamps
/// `last_validated_at`/`updated_at`. `scopes` and `expires_at` are only *set* when
/// the probe supplied them — a probe that returns `None` leaves the previously
/// discovered value in place rather than wiping it.
pub async fn apply_validation(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    outcome: &ValidationOutcome,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let timestamp = to_sql_timestamp(now);
    let scopes_json = outcome
        .scopes
        .as_ref()
        .and_then(|scopes| serde_json::to_string(scopes).ok());
    let expires_at = outcome.expires_at.map(to_sql_timestamp);

    sqlx::query(
        "UPDATE api_credentials SET \
           status            = ?, \
           scopes            = COALESCE(?, scopes), \
           expires_at        = COALESCE(?, expires_at), \
           last_validated_at = ?, \
           updated_at        = ? \
         WHERE id = ?",
    )
    .bind(outcome.status)
    .bind(scopes_json)
    .bind(expires_at)
    .bind(&timestamp)
    .bind(&timestamp)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_and_display_are_redacted() {
        let secret = Secret::new("ghp_supersecrettoken".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert!(!format!("{secret:?}").contains("supersecret"));
        assert!(!format!("{secret}").contains("supersecret"));
        // ...and the one deliberate way back to cleartext still works.
        assert_eq!(secret.expose(), "ghp_supersecrettoken");
    }

    #[test]
    fn last_four_is_the_last_four_characters_only() {
        assert_eq!(last_four("ghp_1234567890abcd"), "abcd");
        assert_eq!(last_four("abcd"), "abcd");
        // Shorter than four: the whole thing, never a panic.
        assert_eq!(last_four("xy"), "xy");
        assert_eq!(last_four(""), "");
    }

    #[test]
    fn providers_round_trip_through_their_spelling() {
        for provider in [
            Provider::Github,
            Provider::Anthropic,
            Provider::Gemini,
            Provider::Smtp,
        ] {
            assert_eq!(provider.as_str().parse::<Provider>().unwrap(), provider);
        }
        assert!("gitlab".parse::<Provider>().is_err());
    }

    #[test]
    fn provider_json_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&Provider::Github).unwrap(),
            "\"github\""
        );
        assert_eq!(
            serde_json::from_str::<Provider>("\"anthropic\"").unwrap(),
            Provider::Anthropic
        );
    }

    #[test]
    fn credential_statuses_round_trip() {
        for status in [
            CredentialStatus::Unchecked,
            CredentialStatus::Valid,
            CredentialStatus::Invalid,
            CredentialStatus::Expired,
        ] {
            assert_eq!(status.as_str().parse::<CredentialStatus>().unwrap(), status);
        }
        assert!("expiring".parse::<CredentialStatus>().is_err());
    }

    #[test]
    fn effective_status_is_expiring_inside_the_window_and_valid_outside_it() {
        let now = crate::auth::now();

        // Valid, no expiry -> valid.
        assert_eq!(
            PillStatus::effective(CredentialStatus::Valid, None, now),
            PillStatus::Valid
        );
        // Valid, expiry far off -> valid.
        assert_eq!(
            PillStatus::effective(
                CredentialStatus::Valid,
                Some(now + Duration::days(EXPIRING_WINDOW_DAYS + 5)),
                now
            ),
            PillStatus::Valid
        );
        // Valid, expiry inside the window -> expiring.
        assert_eq!(
            PillStatus::effective(CredentialStatus::Valid, Some(now + Duration::days(3)), now),
            PillStatus::Expiring
        );
    }

    #[test]
    fn effective_status_reads_a_past_expiry_as_expired_even_if_stored_valid() {
        // The clock is authoritative once the deadline has passed: a credential
        // that was Valid at the last probe but whose expiry is now in the past
        // must surface as Expired without waiting for a re-probe.
        let now = crate::auth::now();
        assert_eq!(
            PillStatus::effective(
                CredentialStatus::Valid,
                Some(now - Duration::seconds(1)),
                now
            ),
            PillStatus::Expired
        );
    }

    #[test]
    fn effective_status_maps_unchecked_and_invalid_straight_across() {
        let now = crate::auth::now();
        assert_eq!(
            PillStatus::effective(CredentialStatus::Unchecked, None, now),
            PillStatus::Unchecked
        );
        assert_eq!(
            PillStatus::effective(CredentialStatus::Invalid, None, now),
            PillStatus::Invalid
        );
    }

    #[test]
    fn scopes_parsing_tolerates_absence_and_garbage() {
        assert_eq!(parse_scopes(None), Vec::<String>::new());
        assert_eq!(parse_scopes(Some("not json")), Vec::<String>::new());
        assert_eq!(
            parse_scopes(Some(r#"["repo","read:user"]"#)),
            vec!["repo".to_owned(), "read:user".to_owned()]
        );
    }
}
