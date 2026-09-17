//! `api_credentials`: encrypted third-party credentials and their metadata.
//!
//! # Database access
//!
//! The runtime `sqlx::query_as::<_, T>("...")` API, as everything outside the
//! macro-free workspace convention does — see `crate::domain`'s module docs for
//! why. Every SQL string here is a `&'static str`.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::vault::Crypto;
use crate::vault::secret::Secret;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Why a provider string could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown credential provider {0:?}")]
pub struct ProviderError(String);

/// The third-party services the vault knows how to hold a credential for.
///
/// A single variant today — GitHub PATs, Phase 12. Claude Code inherits the
/// host's OAuth credentials rather than storing one here (`docker-compose.yml`
/// mounts `~/.claude/.credentials.json` read-only), and Gemini is deferred
/// (`TODO.md`'s Extra section). Adding a variant plus a `CHECK`-free migration
/// row is cheap when a second provider actually needs one — see migration
/// 0009's comment on why the column itself is not a `CHECK` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    GitHub,
}

impl Provider {
    /// The provider's database, JSON and header spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "github",
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
            "github" => Ok(Self::GitHub),
            other => Err(ProviderError(other.to_owned())),
        }
    }
}

// Same sqlx shape as `TagColour` in `crate::domain::tag`: stored as TEXT,
// validated on read.

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
// Status
// ---------------------------------------------------------------------------

/// Validation status of a stored credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    /// Stored, never validated (or validation has not run since the last
    /// replace).
    Unchecked,
    /// The last validation probe succeeded.
    Valid,
    /// The last validation probe was rejected by the provider.
    Invalid,
    /// The provider reported an expiry in the past.
    Expired,
}

impl CredentialStatus {
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

#[derive(Debug, thiserror::Error)]
#[error("unknown credential status {0:?}")]
pub struct CredentialStatusError(String);

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

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// A stored credential, ciphertext and all. Never serialized directly —
/// [`Credential::redacted`] is the API's only window onto it.
#[derive(Debug, Clone, FromRow)]
pub struct Credential {
    pub id: String,
    pub provider: Provider,
    pub label: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    pub last_four: String,
    pub status: CredentialStatus,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The only shape of a credential the API is allowed to return: metadata and
/// the last four characters, never the plaintext or the ciphertext.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedactedCredential {
    pub id: String,
    pub provider: Provider,
    pub label: String,
    pub last_four: String,
    pub status: CredentialStatus,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Credential {
    /// The associated data a ciphertext is bound to: this row's identity, so a
    /// ciphertext copy-pasted onto another row fails to decrypt. See
    /// [`crate::vault::crypto::Crypto::seal`].
    fn aad(id: &str) -> Vec<u8> {
        format!("api_credential:{id}").into_bytes()
    }

    /// Strips the ciphertext and nonce for an API response.
    pub fn redacted(&self) -> RedactedCredential {
        RedactedCredential {
            id: self.id.clone(),
            provider: self.provider,
            label: self.label.clone(),
            last_four: self.last_four.clone(),
            status: self.status,
            last_validated_at: self.last_validated_at,
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
            created_by: self.created_by.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    /// Decrypts the stored secret. The only place plaintext re-enters memory
    /// after being sealed — used solely by a provider's validation probe.
    pub fn decrypt(&self, crypto: &Crypto) -> anyhow::Result<Secret<String>> {
        let sealed = crate::vault::crypto::Sealed {
            nonce: self.nonce.clone(),
            ciphertext: self.ciphertext.clone(),
        };
        let plaintext = crypto.open(&sealed, &Self::aad(&self.id))?;
        Ok(Secret::new(plaintext.to_string()))
    }
}

/// Longest plaintext accepted. GitHub PATs are ~40-93 chars; this is a sanity
/// bound, not a provider-specific one.
pub const MAX_SECRET_LEN: usize = 4096;

/// Shortest plaintext accepted. An empty or near-empty "secret" is a mistake,
/// not a credential.
pub const MIN_SECRET_LEN: usize = 8;

/// Rejects a plaintext too short or too long to plausibly be a real credential.
///
/// # Errors
///
/// [`AppError::Validation`] if `plaintext` is outside `[MIN_SECRET_LEN,
/// MAX_SECRET_LEN]` characters.
pub fn validate_secret_len(plaintext: &str) -> AppResult<()> {
    let len = plaintext.chars().count();
    if !(MIN_SECRET_LEN..=MAX_SECRET_LEN).contains(&len) {
        return Err(AppError::Validation(format!(
            "credential must be between {MIN_SECRET_LEN} and {MAX_SECRET_LEN} characters (got {len})"
        )));
    }
    Ok(())
}

/// The last four characters, for display. Always ASCII-safe: providers issue
/// tokens as ASCII, and this is cosmetic, not a security boundary.
fn last_four(plaintext: &str) -> String {
    let chars: Vec<char> = plaintext.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

// Every SELECT here names the same columns, in the same order, as a literal
// `&'static str` — sqlx 0.9's `SqlSafeStr` bound is only satisfied by a
// `&'static str`, not a `String` built with `format!`, so the column list is
// repeated per query rather than shared through one.

/// Lists every stored credential, newest first.
pub async fn list(db: &Db) -> AppResult<Vec<Credential>> {
    let rows = sqlx::query_as::<_, Credential>(
        "SELECT id, provider, label, ciphertext, nonce, last_four, status, \
         last_validated_at, expires_at, scopes, created_by, created_at, updated_at \
         FROM api_credentials ORDER BY created_at DESC",
    )
    .fetch_all(db.reader())
    .await?;
    Ok(rows)
}

/// Fetches one credential by id.
///
/// # Errors
///
/// [`AppError::NotFound`] if no row has that id.
pub async fn find(db: &Db, id: &str) -> AppResult<Credential> {
    let row = sqlx::query_as::<_, Credential>(
        "SELECT id, provider, label, ciphertext, nonce, last_four, status, \
         last_validated_at, expires_at, scopes, created_by, created_at, updated_at \
         FROM api_credentials WHERE id = ?",
    )
    .bind(id)
    .fetch_one(db.reader())
    .await?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Adds a credential, or replaces the existing one with the same `(provider,
/// label)` — "add/replace" in one call, matching the settings UI's action.
///
/// Replacing resets `status` to `unchecked`: a new secret has not been
/// validated yet, whatever the old one's status was.
pub async fn put(
    db: &Db,
    crypto: &Crypto,
    provider: Provider,
    label: &str,
    plaintext: &str,
    created_by: &str,
    now: DateTime<Utc>,
) -> AppResult<Credential> {
    validate_secret_len(plaintext)?;
    let last_four = last_four(plaintext);

    let mut tx = db.begin_write().await?;

    // A ciphertext is bound to its row's id via AAD (see `Credential::aad`),
    // so which id to seal under depends on whether this is an insert or a
    // replace — checked first, inside the write transaction, so a concurrent
    // put racing on the same (provider, label) cannot insert two rows.
    let existing = sqlx::query_as::<_, Credential>(
        "SELECT id, provider, label, ciphertext, nonce, last_four, status, \
         last_validated_at, expires_at, scopes, created_by, created_at, updated_at \
         FROM api_credentials WHERE provider = ? AND label = ?",
    )
    .bind(provider)
    .bind(label)
    .fetch_optional(&mut *tx)
    .await?;

    let credential = if let Some(existing) = existing {
        let sealed = crypto
            .seal(plaintext, &Credential::aad(&existing.id))
            .map_err(AppError::internal)?;

        sqlx::query(
            "UPDATE api_credentials SET ciphertext = ?, nonce = ?, last_four = ?, \
             status = 'unchecked', last_validated_at = NULL, expires_at = NULL, \
             scopes = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(&last_four)
        .bind(now)
        .bind(&existing.id)
        .execute(&mut *tx)
        .await?;

        Credential {
            ciphertext: sealed.ciphertext,
            nonce: sealed.nonce,
            last_four,
            status: CredentialStatus::Unchecked,
            last_validated_at: None,
            expires_at: None,
            scopes: None,
            updated_at: now,
            ..existing
        }
    } else {
        let id = Uuid::now_v7().to_string();
        let sealed = crypto
            .seal(plaintext, &Credential::aad(&id))
            .map_err(AppError::internal)?;

        sqlx::query(
            "INSERT INTO api_credentials \
             (id, provider, label, ciphertext, nonce, last_four, status, created_by, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'unchecked', ?, ?, ?)",
        )
        .bind(&id)
        .bind(provider)
        .bind(label)
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(&last_four)
        .bind(created_by)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        Credential {
            id,
            provider,
            label: label.to_owned(),
            ciphertext: sealed.ciphertext,
            nonce: sealed.nonce,
            last_four,
            status: CredentialStatus::Unchecked,
            last_validated_at: None,
            expires_at: None,
            scopes: None,
            created_by: created_by.to_owned(),
            created_at: now,
            updated_at: now,
        }
    };

    tx.commit().await?;
    Ok(credential)
}

/// Deletes a credential by id.
///
/// # Errors
///
/// [`AppError::NotFound`] if no row has that id.
pub async fn delete(db: &Db, id: &str) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    let result = sqlx::query("DELETE FROM api_credentials WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

/// The outcome of a validation probe, as far as the vault is concerned.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub status: CredentialStatus,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Option<String>,
}

/// Records the outcome of a validation probe against a credential.
pub async fn record_validation(
    db: &Db,
    id: &str,
    result: &ValidationResult,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE api_credentials SET status = ?, expires_at = ?, scopes = ?, \
         last_validated_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(result.status)
    .bind(result.expires_at)
    .bind(&result.scopes)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db.writer())
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;
    use crate::test_support::TempDb;

    async fn db() -> (Db, TempDb) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        (db, temp)
    }

    async fn seed_user(db: &Db) -> String {
        let mut tx = db.begin_write().await.unwrap();
        let user = crate::auth::user::insert(
            &mut tx,
            &crate::auth::user::NewUser {
                username: "admin".to_owned(),
                email: None,
                display_name: "Admin".to_owned(),
                password_hash: "x".to_owned(),
                role: crate::auth::role::Role::Admin,
                must_change_password: false,
            },
            Utc::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        user.id
    }

    fn crypto() -> Crypto {
        Crypto::from_master(&[3u8; 32]).unwrap()
    }

    #[tokio::test]
    async fn put_then_find_round_trips_and_decrypts() {
        let (db, _temp) = db().await;
        let crypto = crypto();
        let admin = seed_user(&db).await;

        let created = put(
            &db,
            &crypto,
            Provider::GitHub,
            "default",
            "ghp_supersecrettoken1234",
            &admin,
            Utc::now(),
        )
        .await
        .unwrap();

        assert_eq!(created.last_four, "1234");
        assert_eq!(created.status, CredentialStatus::Unchecked);

        let fetched = find(&db, &created.id).await.unwrap();
        let decrypted = fetched.decrypt(&crypto).unwrap();
        assert_eq!(decrypted.expose_secret(), "ghp_supersecrettoken1234");

        db.close().await;
    }

    #[tokio::test]
    async fn putting_the_same_provider_and_label_replaces_not_duplicates() {
        let (db, _temp) = db().await;
        let crypto = crypto();
        let admin = seed_user(&db).await;

        let first = put(
            &db,
            &crypto,
            Provider::GitHub,
            "default",
            "ghp_firsttoken12345678",
            &admin,
            Utc::now(),
        )
        .await
        .unwrap();

        record_validation(
            &db,
            &first.id,
            &ValidationResult {
                status: CredentialStatus::Valid,
                expires_at: None,
                scopes: Some("repo".to_owned()),
            },
            Utc::now(),
        )
        .await
        .unwrap();

        let replaced = put(
            &db,
            &crypto,
            Provider::GitHub,
            "default",
            "ghp_secondtoken1234567",
            &admin,
            Utc::now(),
        )
        .await
        .unwrap();

        assert_eq!(
            replaced.id, first.id,
            "same (provider, label) must replace, not duplicate"
        );
        assert_eq!(replaced.last_four, "4567");
        // Replacing invalidates the old validation — a new secret has not been
        // checked yet, whatever the old one's status was.
        assert_eq!(replaced.status, CredentialStatus::Unchecked);
        assert!(replaced.scopes.is_none());

        let all = list(&db).await.unwrap();
        assert_eq!(all.len(), 1);

        db.close().await;
    }

    #[tokio::test]
    async fn deleting_an_unknown_id_is_not_found() {
        let (db, _temp) = db().await;
        assert!(matches!(
            delete(&db, "nonexistent").await.unwrap_err(),
            AppError::NotFound
        ));
        db.close().await;
    }

    #[tokio::test]
    async fn a_too_short_secret_is_rejected() {
        let (db, _temp) = db().await;
        let crypto = crypto();
        let admin = seed_user(&db).await;

        let err = put(
            &db,
            &crypto,
            Provider::GitHub,
            "default",
            "short",
            &admin,
            Utc::now(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));

        db.close().await;
    }

    #[test]
    fn redacted_never_carries_ciphertext_or_nonce_fields() {
        // Compile-time pin: RedactedCredential has no such fields to leak in
        // the first place — this test exists so the struct definition above
        // cannot silently grow one without a reviewer noticing the comment.
        let credential = Credential {
            id: "1".to_owned(),
            provider: Provider::GitHub,
            label: "default".to_owned(),
            ciphertext: vec![1, 2, 3],
            nonce: vec![4, 5, 6],
            last_four: "1234".to_owned(),
            status: CredentialStatus::Valid,
            last_validated_at: None,
            expires_at: None,
            scopes: None,
            created_by: "u1".to_owned(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&credential.redacted()).unwrap();
        assert!(!json.contains('1') || !json.contains("ciphertext"));
        assert!(!json.to_lowercase().contains("nonce"));
        assert!(!json.to_lowercase().contains("ciphertext"));
    }
}
