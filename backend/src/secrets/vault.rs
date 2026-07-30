//! The vault: the cipher, bound to the rows it protects, plus the probe the
//! provider agents implement to validate a stored credential.
//!
//! # AAD binding — why a ciphertext cannot be swapped between rows
//!
//! Encrypting a secret is not enough on its own. If the ciphertext carried no tie
//! to *which* credential it is, an attacker with write access to the database
//! could copy the GitHub token's ciphertext onto the SMTP row and have Atlas
//! decrypt it there — the cipher would happily oblige, because the bytes are
//! valid under the key.
//!
//! So every seal binds the ciphertext to the row's immutable id as
//! **additional authenticated data**: the id is folded into the Poly1305 tag but
//! not encrypted. Decryption supplies the same id, and the tag only verifies if
//! it matches. Move the ciphertext to another row and the id no longer matches, so
//! it will not open. The id is used rather than `provider + label` because it
//! never changes — a credential's label could in principle be edited, its id
//! cannot.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::error::AppResult;
use crate::secrets::crypto::{Crypto, Sealed};
use crate::secrets::{Credential, CredentialStatus, Provider, Secret};

/// The AAD context prefix. Versioned so a future binding scheme is
/// distinguishable from this one.
const AAD_PREFIX: &str = "atlas.credential.v1:";

/// The encrypted secrets vault.
///
/// Wraps the [`Crypto`] cipher and knows how to bind a ciphertext to the row it
/// belongs to. `Debug` is redacted via [`Crypto`]'s own, so a vault can never
/// print key material.
#[derive(Debug)]
pub struct Vault {
    crypto: Crypto,
}

impl Vault {
    /// Builds a vault directly from a cipher. Mostly for tests; production goes
    /// through [`Vault::from_config`].
    pub fn new(crypto: Crypto) -> Self {
        Self { crypto }
    }

    /// Builds the vault from configuration, or `None` if no master key is set.
    ///
    /// `None` is the honest state of a dev instance with no `ATLAS_MASTER_KEY`:
    /// the vault cannot encrypt, so the credential-writing endpoints refuse
    /// rather than pretend. Production requires the key — `Config::validate`
    /// already rejects `ATLAS_ENV=prod` without one — so `None` cannot happen
    /// there.
    ///
    /// A master key that is present but unusable (not base64) is logged loudly and
    /// treated as absent rather than crashing an already-running process; the
    /// same key would have failed `Config::validate`'s emptiness check only if it
    /// were blank, so this is the backstop for a garbled-but-non-empty value.
    pub fn from_config(config: &Config) -> Option<Self> {
        let master = config.master_key.as_ref()?;
        match Crypto::from_master_b64(master.expose_secret()) {
            Ok(crypto) => Some(Self::new(crypto)),
            Err(err) => {
                tracing::error!(
                    error = ?err,
                    "ATLAS_MASTER_KEY is set but could not derive a vault key; the secrets vault \
                     is disabled and credential endpoints will refuse. Fix the key and restart."
                );
                None
            }
        }
    }

    /// The AAD for a credential id: the string the ciphertext is bound to.
    fn aad(id: &str) -> Vec<u8> {
        format!("{AAD_PREFIX}{id}").into_bytes()
    }

    /// Seals a secret for the credential with id `id`, binding it to that id.
    ///
    /// The id must be the one the row will be inserted with — see
    /// [`crate::secrets::new_id`] and [`crate::secrets::insert`] — or the row
    /// will never decrypt.
    pub fn seal_for(&self, id: &str, secret: &Secret<String>) -> AppResult<Sealed> {
        self.crypto
            .seal(secret.expose().as_bytes(), &Self::aad(id))
            .map_err(crate::error::AppError::internal)
    }

    /// Opens a stored credential's secret.
    ///
    /// # Errors
    ///
    /// A 500 if decryption fails — which, for a row Atlas itself wrote, means the
    /// master key has changed under it, the row was tampered with, or the stored
    /// `key_version` is one this build cannot handle. None of those are the
    /// caller's fault, so the cause is logged and the body is opaque.
    pub fn open(&self, credential: &Credential) -> AppResult<Secret<String>> {
        if credential.key_version != self.crypto.key_version() {
            return Err(crate::error::AppError::internal(anyhow::anyhow!(
                "credential {} was sealed with key_version {}, but this build derives {}; a key \
                 rotation path is needed before it can be opened",
                credential.id,
                credential.key_version,
                self.crypto.key_version()
            )));
        }

        self.crypto
            .open(
                &credential.nonce,
                &credential.ciphertext,
                &Self::aad(&credential.id),
            )
            .map_err(crate::error::AppError::internal)
    }

    /// Opens a secret sealed by [`Vault::seal_for`] under an arbitrary row `id`.
    ///
    /// The credential path goes through [`Vault::open`], which needs a whole
    /// [`Credential`]. A per-repo webhook secret ([`crate::integrations::github`])
    /// is sealed with the same AAD binding but stored in its own columns, so this
    /// opens it from the raw `(id, nonce, ciphertext, key_version)` — the same
    /// key-version refusal and the same id-bound AAD, without inventing a fake
    /// `Credential` around the bytes.
    pub fn open_bytes(
        &self,
        id: &str,
        nonce: &[u8],
        ciphertext: &[u8],
        key_version: i64,
    ) -> AppResult<Secret<String>> {
        if key_version != self.crypto.key_version() {
            return Err(crate::error::AppError::internal(anyhow::anyhow!(
                "row {id} was sealed with key_version {key_version}, but this build derives {}; a \
                 key rotation path is needed before it can be opened",
                self.crypto.key_version()
            )));
        }

        self.crypto
            .open(nonce, ciphertext, &Self::aad(id))
            .map_err(crate::error::AppError::internal)
    }
}

/// What a validation probe discovered about a credential.
///
/// Returned by a [`Validator`] and persisted by
/// [`crate::secrets::apply_validation`]. `scopes` and `expires_at` are `None` when
/// the probe could not determine them — importantly, a missing `expires_at` means
/// *"expiry unknown"*, never *"never expires"* (see `docs/research/corrections.md`
/// #5 for why that distinction is load-bearing for GitHub PATs). `apply_validation`
/// treats `None` as "leave what was there", so an unknown value never wipes a
/// previously discovered one.
#[derive(Debug, Clone)]
pub struct ValidationOutcome {
    /// The status the probe concluded.
    pub status: CredentialStatus,
    /// The provider scopes, if the probe could read them.
    pub scopes: Option<Vec<String>>,
    /// When the credential expires, if the provider reported it.
    pub expires_at: Option<DateTime<Utc>>,
}

impl ValidationOutcome {
    /// The "no probe available" outcome: nothing learned, status unchanged from
    /// `unchecked`. What the stub [`NoopValidator`] returns until a real provider
    /// probe is wired.
    pub fn unchecked() -> Self {
        Self {
            status: CredentialStatus::Unchecked,
            scopes: None,
            expires_at: None,
        }
    }
}

/// A boxed, `Send` future — the return type a dyn-compatible async trait needs.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A provider-specific probe that checks whether a stored secret is still good.
///
/// # For the GitHub and Gemini agents
///
/// This is the seam Phase 12 and Phase 14 build on. Implement it against your
/// provider's cheapest side-effect-free endpoint:
///
/// - **GitHub** (`TODO.md` Phase 12): `GET /user`. Read scopes from the
///   `x-oauth-scopes` header and expiry from `github-authentication-token-expiration`
///   — parsing **both** timezone layouts, and treating a missing header as
///   *unknown* expiry, not *no* expiry (`corrections.md` #3, #5).
/// - **Gemini** (`TODO.md` Phase 14): `GET /v1beta/models`, key in the
///   `x-goog-api-key` header (never the URL).
///
/// Map the result onto [`ValidationOutcome`]: a 200 → `Valid` (+ scopes/expiry);
/// a 401/403 → `Invalid`; an expired token → `Expired`; a transient/network error
/// should surface as a 5xx from your future, not a false `Invalid`.
///
/// The trait is object-safe (it returns a [`BoxFuture`] rather than using
/// `async fn`), so [`default_validator`] can hand back a `Box<dyn Validator>`
/// chosen at runtime by provider.
pub trait Validator: Send + Sync {
    /// Probes `secret` against the provider and reports what it found.
    fn validate<'a>(
        &'a self,
        secret: &'a Secret<String>,
    ) -> BoxFuture<'a, AppResult<ValidationOutcome>>;
}

/// The default probe: does nothing and reports `Unchecked`.
///
/// It exists so the validate endpoint has something to call before any real
/// provider probe is wired: the endpoint still decrypts the secret (proving the
/// vault round-trips) and stamps `last_validated_at`, but claims no validity it
/// cannot actually determine. Phase 12/14 replace it per provider in
/// [`default_validator`].
#[derive(Debug, Default)]
pub struct NoopValidator;

impl Validator for NoopValidator {
    fn validate<'a>(
        &'a self,
        _secret: &'a Secret<String>,
    ) -> BoxFuture<'a, AppResult<ValidationOutcome>> {
        Box::pin(async { Ok(ValidationOutcome::unchecked()) })
    }
}

/// The validator for a provider.
///
/// Every provider maps to [`NoopValidator`] today. When Phase 12 and Phase 14 land
/// their [`Validator`] implementations, this is the one place to route
/// `Provider::Github` / `Provider::Gemini` to them — the API layer already calls
/// through here and needs no change.
pub fn default_validator(provider: Provider) -> Box<dyn Validator> {
    match provider {
        // Phase 12: probe `GET /user`, read scopes/expiry from the response headers.
        Provider::Github => {
            Box::new(crate::integrations::github::validator::GithubValidator::new())
        }
        // Phase 13/14/17 replace these as their probes land.
        Provider::Anthropic | Provider::Gemini | Provider::Smtp => Box::new(NoopValidator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SecretString};

    fn vault() -> Vault {
        Vault::new(
            Crypto::from_master_b64("dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=").unwrap(),
        )
    }

    #[test]
    fn from_config_is_none_without_a_master_key() {
        let config = Config::default();
        assert!(Vault::from_config(&config).is_none());
    }

    #[test]
    fn from_config_builds_a_vault_when_the_master_key_is_set() {
        let config = Config {
            master_key: Some(SecretString::new(
                "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=",
            )),
            ..Config::default()
        };
        assert!(Vault::from_config(&config).is_some());
    }

    #[test]
    fn from_config_disables_the_vault_on_a_garbled_key_rather_than_panicking() {
        let config = Config {
            master_key: Some(SecretString::new("this is not base64 !!!")),
            ..Config::default()
        };
        assert!(Vault::from_config(&config).is_none());
    }

    #[tokio::test]
    async fn the_noop_validator_reports_unchecked() {
        let secret = Secret::new("ghp_token".to_owned());
        let outcome = NoopValidator.validate(&secret).await.unwrap();
        assert_eq!(outcome.status, CredentialStatus::Unchecked);
        assert!(outcome.scopes.is_none());
        assert!(outcome.expires_at.is_none());
    }

    #[test]
    fn a_ciphertext_bound_to_one_id_will_not_open_under_another() {
        // The AAD binding, at the vault level: seal for row A, then try to open a
        // row that carries A's bytes but B's id — it must fail. This is the
        // property a raw `seal`/`open` pair cannot give on its own.
        let vault = vault();
        let secret = Secret::new("ghp_token".to_owned());
        let sealed = vault.seal_for("row-A", &secret).unwrap();

        let as_row_b = Credential {
            id: "row-B".to_owned(),
            provider: Provider::Github,
            label: "test".to_owned(),
            ciphertext: sealed.ciphertext.clone(),
            nonce: sealed.nonce.clone(),
            key_version: sealed.key_version,
            last_four: "oken".to_owned(),
            status: CredentialStatus::Unchecked,
            last_validated_at: None,
            expires_at: None,
            scopes: None,
            created_by: None,
            created_at: crate::auth::now(),
            updated_at: crate::auth::now(),
        };
        assert!(
            vault.open(&as_row_b).is_err(),
            "a ciphertext must not decrypt under a different row's id"
        );

        // ...and the same bytes under the correct id do open.
        let as_row_a = Credential {
            id: "row-A".to_owned(),
            ..as_row_b
        };
        let opened = vault.open(&as_row_a).unwrap();
        assert_eq!(opened.expose(), "ghp_token");
    }

    #[test]
    fn a_future_key_version_is_refused_rather_than_misdecrypted() {
        let vault = vault();
        let secret = Secret::new("ghp_token".to_owned());
        let sealed = vault.seal_for("row-A", &secret).unwrap();

        let bumped = Credential {
            id: "row-A".to_owned(),
            provider: Provider::Github,
            label: "test".to_owned(),
            ciphertext: sealed.ciphertext,
            nonce: sealed.nonce,
            key_version: 999,
            last_four: "oken".to_owned(),
            status: CredentialStatus::Unchecked,
            last_validated_at: None,
            expires_at: None,
            scopes: None,
            created_by: None,
            created_at: crate::auth::now(),
            updated_at: crate::auth::now(),
        };
        assert!(vault.open(&bumped).is_err());
    }
}
