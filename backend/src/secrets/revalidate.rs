//! Scheduled credential revalidation and the validation-probe rate limit (`TODO.md` Phase 11).
//!
//! A PAT revoked on GitHub, or a Gemini key whose project was deleted, should be found by
//! Atlas on a quiet night — not by the agent run that needed it. The scheduled check probes
//! every credential not checked in [`MAX_AGE`], records the outcome exactly as a manual
//! "Validate" does, and tells every instance admin when one *becomes* unusable (or starts to
//! expire within [`EXPIRY_WARNING`]) — once per change, not every night.

use chrono::{DateTime, Duration, Utc};

use crate::activity::Hub;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::notify::{self, Kind, Notice};
use crate::secrets::vault::{Vault, default_validator};
use crate::secrets::{self, Credential, CredentialStatus};

/// How stale a check may be before the scheduler probes again.
pub const MAX_AGE: Duration = Duration::hours(24);

/// The least time between two probes of one credential. A probe is a real request to the
/// provider; a button mashed (or a script looping) must not become a burst against
/// GitHub's or Google's auth endpoints from this instance's address.
pub const PROBE_INTERVAL: Duration = Duration::seconds(30);

/// How far ahead an expiry is worth a warning.
pub const EXPIRY_WARNING: Duration = Duration::days(7);

/// Refuses a probe that would come too soon after the last one.
pub fn check_rate(credential: &Credential, now: DateTime<Utc>) -> AppResult<()> {
    if let Some(last) = credential.last_validated_at {
        let wait = PROBE_INTERVAL - (now - last);
        if wait > Duration::zero() {
            return Err(AppError::TooManyRequests(format!(
                "This credential was checked {} seconds ago; try again in {} seconds.",
                (now - last).num_seconds().max(0),
                wait.num_seconds().max(1)
            )));
        }
    }
    Ok(())
}

/// What changed for one credential.
#[derive(Debug, Clone)]
pub struct Change {
    /// The credential's label.
    pub label: String,
    /// Its provider.
    pub provider: String,
    /// Before.
    pub before: CredentialStatus,
    /// After.
    pub after: CredentialStatus,
    /// Expires within [`EXPIRY_WARNING`] (and did not before this check).
    pub expiring: Option<DateTime<Utc>>,
}

impl Change {
    /// Whether an admin should hear about it.
    pub fn is_news(&self) -> bool {
        let broke = matches!(
            self.after,
            CredentialStatus::Invalid | CredentialStatus::Expired
        ) && self.before != self.after;
        broke || self.expiring.is_some()
    }
}

/// Probes every credential last checked more than [`MAX_AGE`] ago (or never).
pub async fn run(db: &Db, vault: &Vault, now: DateTime<Utc>) -> AppResult<Vec<Change>> {
    let mut changes = Vec::new();
    for credential in secrets::list(db).await? {
        if credential
            .last_validated_at
            .is_some_and(|last| now - last < MAX_AGE)
        {
            continue;
        }
        let secret = match vault.open(&credential) {
            Ok(secret) => secret,
            Err(err) => {
                tracing::warn!(credential_id = %credential.id, error = %err, "could not open a credential for revalidation");
                continue;
            }
        };
        let outcome = match default_validator(credential.provider)
            .validate(&secret)
            .await
        {
            Ok(outcome) => outcome,
            // A network failure says nothing about the key: try again next time.
            Err(err) => {
                tracing::warn!(credential_id = %credential.id, error = %err, "credential revalidation probe failed");
                continue;
            }
        };
        let mut tx = db.begin_write().await?;
        secrets::apply_validation(&mut tx, &credential.id, &outcome, now).await?;
        tx.commit().await?;

        let expires = outcome.expires_at.or(credential.expires_at);
        let soon = |at: Option<DateTime<Utc>>, from: DateTime<Utc>| {
            at.is_some_and(|at| at > from && at - from <= EXPIRY_WARNING)
        };
        // Warn once: when this check is the first to see it inside the window.
        let was_warned = credential
            .last_validated_at
            .is_some_and(|last| soon(credential.expires_at, last));
        changes.push(Change {
            label: credential.label.clone(),
            provider: credential.provider.to_string(),
            before: credential.status,
            after: outcome.status,
            expiring: (soon(expires, now) && !was_warned)
                .then_some(expires)
                .flatten(),
        });
    }
    Ok(changes)
}

/// Tells every active admin about the changes that matter.
pub async fn notify_admins(db: &Db, hub: &Hub, changes: &[Change]) -> AppResult<()> {
    let admins: Vec<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' AND is_active = 1")
            .fetch_all(db.reader())
            .await?;
    for change in changes.iter().filter(|c| c.is_news()) {
        let title = match (change.after, change.expiring) {
            (CredentialStatus::Invalid, _) => format!(
                "The {} credential \"{}\" was rejected by its provider",
                change.provider, change.label
            ),
            (CredentialStatus::Expired, _) => format!(
                "The {} credential \"{}\" has expired",
                change.provider, change.label
            ),
            (_, Some(at)) => format!(
                "The {} credential \"{}\" expires on {}",
                change.provider,
                change.label,
                at.format("%Y-%m-%d")
            ),
            _ => continue,
        };
        for admin in &admins {
            notify::deliver(
                db,
                hub,
                Notice {
                    user_id: admin.clone(),
                    kind: Kind::System,
                    card_id: None,
                    project_id: None,
                    actor_id: None,
                    title: title.clone(),
                    body: Some(
                        "Replace it under Settings → Credentials; runs that need it will fail \
                         until then."
                            .to_owned(),
                    ),
                    link: Some("/settings/credentials".to_owned()),
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(before: CredentialStatus, after: CredentialStatus) -> Change {
        Change {
            label: "main".into(),
            provider: "github".into(),
            before,
            after,
            expiring: None,
        }
    }

    #[test]
    fn only_a_change_for_the_worse_is_news() {
        assert!(change(CredentialStatus::Valid, CredentialStatus::Invalid).is_news());
        assert!(change(CredentialStatus::Valid, CredentialStatus::Expired).is_news());
        assert!(!change(CredentialStatus::Invalid, CredentialStatus::Invalid).is_news());
        assert!(!change(CredentialStatus::Unchecked, CredentialStatus::Valid).is_news());
    }
}
