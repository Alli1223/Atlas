//! Outbound email: SMTP settings, the pending-notification mailer, and the daily digest.
//!
//! The SMTP password is a vault credential (provider `smtp`), so it is sealed at rest and
//! never returned over the API like every other secret; `smtp_settings` holds only the
//! non-secret half (host, port, user, from, TLS mode) and points at the credential.
//!
//! Mail is sent by [`flush`] on a schedule, never from a request handler: an SMTP server that
//! takes thirty seconds to answer must not make a comment take thirty seconds to post.

use std::fmt::Write as _;

use chrono::{DateTime, Duration, Utc};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::secrets::{self, Vault};

/// How the connection to the SMTP server is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Plain connection upgraded with STARTTLS (port 587). Required, not opportunistic.
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption. Only for a relay on localhost.
    None,
}

impl TlsMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starttls => "starttls",
            Self::Tls => "tls",
            Self::None => "none",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "tls" => Self::Tls,
            "none" => Self::None,
            _ => Self::Starttls,
        }
    }
}

/// The non-secret SMTP settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SmtpSettings {
    /// The server.
    pub host: String,
    /// Its port.
    pub port: u16,
    /// The login, if the server wants one.
    pub username: Option<String>,
    /// `Atlas <atlas@example.com>` or a bare address.
    pub from_address: String,
    /// How to secure the connection.
    pub tls: TlsMode,
    /// The vault credential (provider `smtp`) holding the password, if any.
    pub password_credential_id: Option<String>,
}

#[derive(Debug, FromRow)]
struct SettingsRow {
    host: String,
    port: i64,
    username: Option<String>,
    from_address: String,
    tls: String,
    password_credential_id: Option<String>,
}

/// The stored SMTP settings, if any.
pub async fn settings(db: &Db) -> AppResult<Option<SmtpSettings>> {
    let row = sqlx::query_as::<_, SettingsRow>(
        "SELECT host, port, username, from_address, tls, password_credential_id \
         FROM smtp_settings WHERE id = 1",
    )
    .fetch_optional(db.reader())
    .await?;
    Ok(row.map(|r| SmtpSettings {
        host: r.host,
        port: u16::try_from(r.port).unwrap_or(587),
        username: r.username,
        from_address: r.from_address,
        tls: TlsMode::parse(&r.tls),
        password_credential_id: r.password_credential_id,
    }))
}

/// Checks and stores the SMTP settings.
pub async fn save(db: &Db, value: &SmtpSettings, now: DateTime<Utc>) -> AppResult<()> {
    let host = value.host.trim();
    if host.is_empty() || host.len() > 253 || host.contains(['/', ' ', '@']) {
        return Err(AppError::Validation(
            "Enter the SMTP server's host name.".to_owned(),
        ));
    }
    if value.port == 0 {
        return Err(AppError::Validation(
            "Enter the SMTP server's port.".to_owned(),
        ));
    }
    value.from_address.parse::<Mailbox>().map_err(|_| {
        AppError::Validation(format!("{:?} is not an email address.", value.from_address))
    })?;
    if let Some(id) = &value.password_credential_id {
        let credential = secrets::find_by_id(db, id)
            .await?
            .ok_or_else(|| AppError::Validation("No such credential.".to_owned()))?;
        if credential.provider != secrets::Provider::Smtp {
            return Err(AppError::Validation(
                "The password must be an SMTP credential.".to_owned(),
            ));
        }
    }
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "INSERT INTO smtp_settings (id, host, port, username, from_address, tls, \
         password_credential_id, updated_at) VALUES (1, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET host = excluded.host, port = excluded.port, \
         username = excluded.username, from_address = excluded.from_address, tls = excluded.tls, \
         password_credential_id = excluded.password_credential_id, updated_at = excluded.updated_at",
    )
    .bind(host)
    .bind(i64::from(value.port))
    .bind(value.username.as_deref().map(str::trim).filter(|u| !u.is_empty()))
    .bind(value.from_address.trim())
    .bind(value.tls.as_str())
    .bind(&value.password_credential_id)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes the SMTP settings, which turns email off.
pub async fn clear(db: &Db) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query("DELETE FROM smtp_settings")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A transport built from the settings, with the password opened from the vault.
async fn transport(
    db: &Db,
    vault: Option<&Vault>,
    settings: &SmtpSettings,
) -> AppResult<AsyncSmtpTransport<Tokio1Executor>> {
    let builder = match settings.tls {
        TlsMode::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
        TlsMode::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host),
        TlsMode::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &settings.host,
        )),
    }
    .map_err(|err| AppError::Validation(format!("SMTP setup failed: {err}")))?
    .port(settings.port)
    .timeout(Some(std::time::Duration::from_secs(20)));

    let builder = match (&settings.username, &settings.password_credential_id) {
        (Some(username), Some(credential_id)) => {
            let vault = vault.ok_or_else(|| {
                AppError::Validation("The vault is sealed (no ATLAS_MASTER_KEY).".to_owned())
            })?;
            let credential = secrets::find_by_id(db, credential_id)
                .await?
                .ok_or_else(|| AppError::Validation("The SMTP credential is gone.".to_owned()))?;
            let password = vault.open(&credential)?;
            builder.credentials(Credentials::new(
                username.clone(),
                password.expose().clone(),
            ))
        }
        _ => builder,
    };
    Ok(builder.build())
}

/// Sends one plain-text email.
pub async fn send(
    db: &Db,
    vault: Option<&Vault>,
    to: &str,
    subject: &str,
    body: &str,
) -> AppResult<()> {
    let settings = settings(db)
        .await?
        .ok_or_else(|| AppError::Validation("SMTP is not configured.".to_owned()))?;
    let from: Mailbox = settings
        .from_address
        .parse()
        .map_err(|_| AppError::Validation("The from address is invalid.".to_owned()))?;
    let to: Mailbox = to
        .parse()
        .map_err(|_| AppError::Validation(format!("{to:?} is not an email address.")))?;
    let message = Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_owned())
        .map_err(|err| AppError::internal(anyhow::anyhow!("failed to build an email: {err}")))?;
    transport(db, vault, &settings)
        .await?
        .send(message)
        .await
        .map_err(|err| AppError::Validation(format!("The SMTP server refused the email: {err}")))?;
    Ok(())
}

#[derive(Debug, FromRow)]
struct Pending {
    id: String,
    user_id: String,
    email: Option<String>,
    title: String,
    body: Option<String>,
    link: Option<String>,
    count: i64,
}

/// The public base for links in emails: `ATLAS_PUBLIC_URL`, or nothing (relative paths).
fn absolute(public_url: Option<&str>, link: Option<&str>) -> String {
    match (public_url, link) {
        (Some(base), Some(path)) => format!("{}{path}", base.trim_end_matches('/')),
        (None, Some(path)) => path.to_owned(),
        _ => String::new(),
    }
}

/// Sends every pending notification email, one message per user, and clears the flag.
///
/// With no SMTP configured the queue is simply cleared — a notice nobody can email is not
/// worth keeping pending forever. Users with no address are skipped the same way. Returns how
/// many emails went out.
pub async fn flush(db: &Db, vault: Option<&Vault>, public_url: Option<&str>) -> AppResult<usize> {
    let pending = sqlx::query_as::<_, Pending>(
        "SELECT n.id, n.user_id, u.email, n.title, n.body, n.link, n.count \
           FROM notifications n JOIN users u ON u.id = n.user_id \
          WHERE n.email_pending = 1 ORDER BY n.user_id, n.updated_at LIMIT 500",
    )
    .fetch_all(db.reader())
    .await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let configured = settings(db).await?.is_some();

    let mut sent = 0;
    let mut start = 0;
    while start < pending.len() {
        let user_id = &pending[start].user_id;
        let end = pending[start..]
            .iter()
            .position(|p| &p.user_id != user_id)
            .map_or(pending.len(), |offset| start + offset);
        let batch = &pending[start..end];
        if configured && let Some(address) = batch[0].email.as_deref() {
            let subject = if batch.len() == 1 {
                batch[0].title.clone()
            } else {
                format!("{} new notifications", batch.len())
            };
            let mut text = String::new();
            for item in batch {
                text.push_str(&item.title);
                if item.count > 1 {
                    let _ = write!(text, " ({} updates)", item.count);
                }
                text.push('\n');
                if let Some(body) = &item.body {
                    text.push_str(body);
                    text.push('\n');
                }
                let link = absolute(public_url, item.link.as_deref());
                if !link.is_empty() {
                    text.push_str(&link);
                    text.push('\n');
                }
                text.push('\n');
            }
            text.push_str(
                "— Atlas. Change what you are emailed about in your notification settings.\n",
            );
            match send(db, vault, address, &subject, &text).await {
                Ok(()) => sent += 1,
                Err(err) => {
                    // Leave them pending: a transient SMTP outage retries on the next tick.
                    tracing::warn!(error = %err, "failed to send a notification email");
                    start = end;
                    continue;
                }
            }
        }
        let mut tx = db.begin_write().await?;
        for item in batch {
            sqlx::query("UPDATE notifications SET email_pending = 0 WHERE id = ?")
                .bind(&item.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        start = end;
    }
    Ok(sent)
}

#[derive(Debug, FromRow)]
struct DigestUser {
    user_id: String,
    email: Option<String>,
    last_digest_at: Option<DateTime<Utc>>,
}

/// Sends each opted-in user a digest of what they have not read since their last one, at most
/// once a day. Returns how many digests went out.
pub async fn digest(
    db: &Db,
    vault: Option<&Vault>,
    public_url: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<usize> {
    if settings(db).await?.is_none() {
        return Ok(0);
    }
    let due = to_sql_timestamp(now - Duration::hours(23));
    let users = sqlx::query_as::<_, DigestUser>(
        "SELECT s.user_id, u.email, s.last_digest_at FROM notification_settings s \
           JOIN users u ON u.id = s.user_id \
          WHERE s.daily_digest = 1 AND u.is_active = 1 AND u.email IS NOT NULL \
            AND (s.last_digest_at IS NULL OR s.last_digest_at <= ?)",
    )
    .bind(&due)
    .fetch_all(db.reader())
    .await?;

    let mut sent = 0;
    for user in users {
        let since = user.last_digest_at.unwrap_or(now - Duration::days(1));
        let items = sqlx::query_as::<_, (String, Option<String>, i64)>(
            "SELECT title, link, count FROM notifications \
              WHERE user_id = ? AND read_at IS NULL AND updated_at > ? \
              ORDER BY updated_at DESC LIMIT 100",
        )
        .bind(&user.user_id)
        .bind(to_sql_timestamp(since))
        .fetch_all(db.reader())
        .await?;
        if !items.is_empty()
            && let Some(address) = &user.email
        {
            let mut text = format!("You have {} unread notification(s):\n\n", items.len());
            for (title, link, count) in &items {
                text.push_str("• ");
                text.push_str(title);
                if *count > 1 {
                    let _ = write!(text, " ({count} updates)");
                }
                let link = absolute(public_url, link.as_deref());
                if !link.is_empty() {
                    let _ = write!(text, "\n  {link}");
                }
                text.push('\n');
            }
            if let Err(err) = send(db, vault, address, "Your Atlas daily digest", &text).await {
                tracing::warn!(error = %err, "failed to send a daily digest");
                continue;
            }
            sent += 1;
        }
        let mut tx = db.begin_write().await?;
        sqlx::query("UPDATE notification_settings SET last_digest_at = ? WHERE user_id = ?")
            .bind(to_sql_timestamp(now))
            .bind(&user.user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_in_emails_are_absolute_only_when_the_public_url_is_known() {
        assert_eq!(
            absolute(Some("https://atlas.example.com/"), Some("/cards/A-1")),
            "https://atlas.example.com/cards/A-1"
        );
        assert_eq!(absolute(None, Some("/cards/A-1")), "/cards/A-1");
        assert_eq!(absolute(Some("https://x"), None), "");
    }

    #[test]
    fn tls_modes_round_trip_and_default_to_starttls() {
        for mode in [TlsMode::Starttls, TlsMode::Tls, TlsMode::None] {
            assert_eq!(TlsMode::parse(mode.as_str()), mode);
        }
        assert_eq!(TlsMode::parse("junk"), TlsMode::Starttls);
    }
}
