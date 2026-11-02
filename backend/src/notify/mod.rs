//! Notifications: the bell, per-user preferences, email, and the daily digest.
//!
//! # From events to notices
//!
//! [`on_event`] turns one [`crate::activity::Event`] into notices for the people who should
//! hear about it:
//!
//! | event | who |
//! |---|---|
//! | assignee set | the new assignee (`assigned`) |
//! | status changed | assignee, reporter, watchers (`transitioned`) |
//! | other field changed | watchers (`updated`) |
//! | comment posted | assignee, reporter, watchers (`commented`) |
//! | `@username` in a comment or description | that user (`mentioned`) |
//!
//! Every notice then passes through [`deliver`], which applies, in order:
//!
//! 1. **Access** — someone who can no longer see the card hears nothing about it.
//! 2. **"Don't notify me of my own changes"** — on by default; a solo user must not be spammed
//!    by themselves (TODO.md Phase 17's table stakes). Opt-in per user to hear them anyway.
//! 3. **Preferences** — per kind: `in_app`, `email` (bell *and* email), or `off`.
//! 4. **Coalescing** — an unread notice of the same kind on the same card within
//!    [`COALESCE_WINDOW`] is bumped (`count + 1`, newest body) instead of duplicated, so a burst
//!    of five edits is one "5 updates" row.
//!
//! Agent runs notify their starter directly through [`deliver`] (`agent`), since an agent
//! finishing is not a card change.

pub mod mail;

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::activity::{Event, EventKind, Hub, HubMessage};
use crate::auth::{to_sql_timestamp, user};
use crate::db::Db;
use crate::domain::{links, member, project};
use crate::error::{AppError, AppResult};

/// How long an unread notice keeps absorbing same-kind events on the same card.
pub const COALESCE_WINDOW: Duration = Duration::minutes(15);

/// The kinds of notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// You were made the assignee.
    Assigned,
    /// Someone `@`-mentioned you.
    Mentioned,
    /// A comment on a card you are involved in.
    Commented,
    /// A card you are involved in changed status.
    Transitioned,
    /// A card you watch changed.
    Updated,
    /// A Claude Code run you started finished, failed or needs you.
    Agent,
    /// Instance administration: a credential failed its scheduled check, a backup failed.
    System,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Self; 7] = [
        Self::Assigned,
        Self::Mentioned,
        Self::Commented,
        Self::Transitioned,
        Self::Updated,
        Self::Agent,
        Self::System,
    ];

    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::Mentioned => "mentioned",
            Self::Commented => "commented",
            Self::Transitioned => "transitioned",
            Self::Updated => "updated",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }

    /// Parses the stored spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == value)
    }
}

/// Where a kind of notice goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The bell only.
    InApp,
    /// The bell and an email.
    Email,
    /// Nowhere.
    Off,
}

impl Channel {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InApp => "in_app",
            Self::Email => "email",
            Self::Off => "off",
        }
    }

    /// Parses the stored spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "in_app" => Some(Self::InApp),
            "email" => Some(Self::Email),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// The channel a kind uses when the user has not chosen.
///
/// Being assigned or mentioned is addressed to you personally; everything else is ambient.
/// Neither emails by default — email is an opt-in, because an instance may have no SMTP at all.
pub fn default_channel(_kind: Kind) -> Channel {
    Channel::InApp
}

/// A notice, before delivery decides whether and how it lands.
#[derive(Debug, Clone)]
pub struct Notice {
    /// Who it is for.
    pub user_id: String,
    /// What kind.
    pub kind: Kind,
    /// The card it is about, if any.
    pub card_id: Option<String>,
    /// The project the card is in (for the access check), if any.
    pub project_id: Option<String>,
    /// Who caused it.
    pub actor_id: Option<String>,
    /// One line.
    pub title: String,
    /// Detail.
    pub body: Option<String>,
    /// An in-app path.
    pub link: Option<String>,
}

/// A notification as the bell shows it.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// UUID v7.
    pub id: String,
    /// `assigned`, `mentioned`, ….
    pub kind: String,
    /// The card, if any.
    pub card_id: Option<String>,
    /// The card's key, if any.
    pub card_key: Option<String>,
    /// Who caused it.
    pub actor_id: Option<String>,
    /// Their display name.
    pub actor_name: Option<String>,
    /// One line.
    pub title: String,
    /// Detail.
    pub body: Option<String>,
    /// An in-app path.
    pub link: Option<String>,
    /// How many events were coalesced into it.
    pub count: i64,
    /// When it was read. `None` = unread.
    pub read_at: Option<DateTime<Utc>>,
    /// When it first arrived.
    pub created_at: DateTime<Utc>,
    /// When it last absorbed an event.
    pub updated_at: DateTime<Utc>,
}

/// A user's notification settings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    /// Hear about your own changes too. Off by default.
    pub notify_own_changes: bool,
    /// A daily email of what you have not read.
    pub daily_digest: bool,
    /// Per-kind delivery.
    pub channels: Vec<KindChannel>,
}

/// One kind's channel.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KindChannel {
    /// The kind.
    pub kind: Kind,
    /// Where it goes.
    pub channel: Channel,
}

/// `@username` mentions in markdown, in order of first appearance.
///
/// A mention starts at the beginning of the text or after a character that cannot be part of a
/// word — so `alice@example.com` is an email address, not a mention of `example.com`. Code
/// spans and fenced blocks are skipped: `@decorator` in a Python snippet is not a person.
pub fn mentions(markdown: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut in_code = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '`' {
                in_code = !in_code;
            } else if c == '@'
                && !in_code
                && (i == 0
                    || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '.' | '_' | '-')))
            {
                let name: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    .collect();
                let name = name.trim_end_matches(['.', '-']).to_owned();
                let consumed = 1 + name.chars().count();
                if !name.is_empty() && !found.iter().any(|f| f.eq_ignore_ascii_case(&name)) {
                    found.push(name);
                }
                i += consumed;
                continue;
            }
            i += 1;
        }
    }
    found
}

/// The mentions in `after` that were not already in `before` — so editing a description that
/// already mentions Alice does not mention her again.
pub fn new_mentions(before: Option<&str>, after: Option<&str>) -> Vec<String> {
    let old = before.map(mentions).unwrap_or_default();
    after
        .map(mentions)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !old.iter().any(|o| o.eq_ignore_ascii_case(m)))
        .collect()
}

async fn user_ids_by_username(db: &Db, usernames: &[String]) -> AppResult<Vec<String>> {
    let mut ids = Vec::new();
    for name in usernames {
        if let Some(found) = user::find_by_username(db, name).await?
            && found.is_active
        {
            ids.push(found.id);
        }
    }
    Ok(ids)
}

async fn display_name(db: &Db, user_id: Option<&str>) -> AppResult<String> {
    let Some(id) = user_id else {
        return Ok("Atlas".to_owned());
    };
    Ok(user::find_by_id(db, id)
        .await?
        .map_or_else(|| "Someone".to_owned(), |u| u.display_name))
}

/// Turns one activity event into notices and delivers them.
// One arm per event kind, each implementing one row of the module doc's table; the table is
// the spec and this is it, row for row.
#[allow(clippy::too_many_lines)]
pub async fn on_event(db: &Db, hub: &Hub, event: &Event) -> AppResult<()> {
    let card = &event.card;
    let who = display_name(db, event.author_id.as_deref()).await?;
    let link = Some(format!("/cards/{}", card.key));
    let watchers: BTreeSet<String> = links::watcher_ids(db, &card.id)
        .await?
        .into_iter()
        .collect();
    let involved: BTreeSet<String> = watchers
        .iter()
        .cloned()
        .chain(card.assignee_id.clone())
        .chain(card.reporter_id.clone())
        .collect();

    let mut notices: Vec<Notice> = Vec::new();
    let notice = |user_id: &str, kind: Kind, title: String, body: Option<String>| Notice {
        user_id: user_id.to_owned(),
        kind,
        card_id: Some(card.id.clone()),
        project_id: Some(card.project_id.clone()),
        actor_id: event.author_id.clone(),
        title,
        body,
        link: link.clone(),
    };

    match &event.kind {
        EventKind::CardCreated => {
            if let Some(assignee) = &card.assignee_id {
                notices.push(notice(
                    assignee,
                    Kind::Assigned,
                    format!("{who} assigned you {}", card.key),
                    Some(card.summary.clone()),
                ));
            }
            for id in user_ids_by_username(db, &mentions(card.description.as_deref().unwrap_or("")))
                .await?
            {
                notices.push(notice(
                    &id,
                    Kind::Mentioned,
                    format!("{who} mentioned you in {}", card.key),
                    Some(card.summary.clone()),
                ));
            }
        }
        EventKind::CardUpdated { changes } => {
            let mut told: BTreeSet<String> = BTreeSet::new();
            if let Some(change) = event.change("assignee")
                && let Some(assignee) = &change.to_value
            {
                notices.push(notice(
                    assignee,
                    Kind::Assigned,
                    format!("{who} assigned you {}", card.key),
                    Some(card.summary.clone()),
                ));
                told.insert(assignee.clone());
            }
            if let Some(change) = event.change("description") {
                for id in user_ids_by_username(
                    db,
                    &new_mentions(change.from_value.as_deref(), change.to_value.as_deref()),
                )
                .await?
                {
                    notices.push(notice(
                        &id,
                        Kind::Mentioned,
                        format!("{who} mentioned you in {}", card.key),
                        Some(card.summary.clone()),
                    ));
                    told.insert(id);
                }
            }
            if let Some(change) = event.change("status") {
                let to = change.to_display.clone().unwrap_or_default();
                for id in involved.iter().filter(|id| !told.contains(*id)) {
                    notices.push(notice(
                        id,
                        Kind::Transitioned,
                        format!("{who} moved {} to {to}", card.key),
                        Some(card.summary.clone()),
                    ));
                }
            } else {
                // Rank-only changes are a board drag within a column: noise, not news.
                let meaningful: Vec<&str> = changes
                    .iter()
                    .map(|c| c.field.as_str())
                    .filter(|f| *f != "rank")
                    .collect();
                if !meaningful.is_empty() {
                    let fields = meaningful.join(", ");
                    for id in watchers.iter().filter(|id| !told.contains(*id)) {
                        notices.push(notice(
                            id,
                            Kind::Updated,
                            format!("{who} updated {}", card.key),
                            Some(format!("Changed: {fields}")),
                        ));
                    }
                }
            }
        }
        EventKind::CommentAdded { body, .. } => {
            let excerpt: String = body.chars().take(280).collect();
            let mentioned = user_ids_by_username(db, &mentions(body)).await?;
            for id in &mentioned {
                notices.push(notice(
                    id,
                    Kind::Mentioned,
                    format!("{who} mentioned you on {}", card.key),
                    Some(excerpt.clone()),
                ));
            }
            for id in involved.iter().filter(|id| !mentioned.contains(*id)) {
                notices.push(notice(
                    id,
                    Kind::Commented,
                    format!("{who} commented on {}", card.key),
                    Some(excerpt.clone()),
                ));
            }
        }
    }

    for notice in notices {
        deliver(db, hub, notice).await?;
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct Settings {
    notify_own_changes: bool,
    daily_digest: bool,
}

async fn settings(db: &Db, user_id: &str) -> AppResult<Settings> {
    Ok(sqlx::query_as::<_, Settings>(
        "SELECT notify_own_changes, daily_digest FROM notification_settings WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db.reader())
    .await?
    .unwrap_or(Settings {
        notify_own_changes: false,
        daily_digest: false,
    }))
}

async fn channel_for(db: &Db, user_id: &str, kind: Kind) -> AppResult<Channel> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT channel FROM notification_prefs WHERE user_id = ? AND kind = ?")
            .bind(user_id)
            .bind(kind.as_str())
            .fetch_optional(db.reader())
            .await?;
    Ok(stored
        .as_deref()
        .and_then(Channel::parse)
        .unwrap_or_else(|| default_channel(kind)))
}

/// What [`deliver`] did with a notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Dropped: no access, your own change, turned off, or an inactive account.
    Suppressed,
    /// A new row.
    Created(String),
    /// Folded into an existing unread row.
    Coalesced(String),
}

/// Applies access, self-suppression, preferences and coalescing, then stores the notice and
/// pushes it to the user's open sockets.
pub async fn deliver(db: &Db, hub: &Hub, notice: Notice) -> AppResult<Delivery> {
    let Some(recipient) = user::find_by_id(db, &notice.user_id).await? else {
        return Ok(Delivery::Suppressed);
    };
    if !recipient.is_active {
        return Ok(Delivery::Suppressed);
    }
    if let Some(project_id) = &notice.project_id {
        let Some(project) = project::find_by_id(db, project_id).await? else {
            return Ok(Delivery::Suppressed);
        };
        if member::effective_role(db, &project, &recipient)
            .await?
            .is_none()
        {
            return Ok(Delivery::Suppressed);
        }
    }
    if notice.actor_id.as_deref() == Some(notice.user_id.as_str())
        && !settings(db, &notice.user_id).await?.notify_own_changes
    {
        return Ok(Delivery::Suppressed);
    }
    let channel = channel_for(db, &notice.user_id, notice.kind).await?;
    if channel == Channel::Off {
        return Ok(Delivery::Suppressed);
    }
    let email = channel == Channel::Email;

    let now = crate::auth::now();
    let since = to_sql_timestamp(now - COALESCE_WINDOW);
    let mut tx = db.begin_write().await?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM notifications \
          WHERE user_id = ? AND kind = ? AND card_id IS ? AND read_at IS NULL \
            AND updated_at >= ? \
          ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(&notice.user_id)
    .bind(notice.kind.as_str())
    .bind(&notice.card_id)
    .bind(&since)
    .fetch_optional(&mut *tx)
    .await?;

    let outcome = if let Some(id) = existing {
        sqlx::query(
            "UPDATE notifications SET count = count + 1, title = ?, body = ?, actor_id = ?, \
             email_pending = MAX(email_pending, ?), updated_at = ? WHERE id = ?",
        )
        .bind(&notice.title)
        .bind(&notice.body)
        .bind(&notice.actor_id)
        .bind(email)
        .bind(to_sql_timestamp(now))
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        Delivery::Coalesced(id)
    } else {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO notifications (id, user_id, kind, card_id, actor_id, title, body, link, \
             count, email_pending, read_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, NULL, ?, ?)",
        )
        .bind(&id)
        .bind(&notice.user_id)
        .bind(notice.kind.as_str())
        .bind(&notice.card_id)
        .bind(&notice.actor_id)
        .bind(&notice.title)
        .bind(&notice.body)
        .bind(&notice.link)
        .bind(email)
        .bind(to_sql_timestamp(now))
        .bind(to_sql_timestamp(now))
        .execute(&mut *tx)
        .await?;
        Delivery::Created(id)
    };
    tx.commit().await?;

    hub.publish(HubMessage {
        project_id: None,
        user_id: Some(notice.user_id.clone()),
        payload: json!({
            "type": "notification",
            "kind": notice.kind.as_str(),
            "title": notice.title,
            "link": notice.link,
        }),
    });
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Reading and preferences
// ---------------------------------------------------------------------------

/// A user's most recent notifications, newest first.
pub async fn list(
    db: &Db,
    user_id: &str,
    unread_only: bool,
    limit: i64,
) -> AppResult<Vec<Notification>> {
    Ok(sqlx::query_as::<_, Notification>(
        "SELECT n.id, n.kind, n.card_id, c.key AS card_key, n.actor_id, \
                u.display_name AS actor_name, n.title, n.body, n.link, n.count, n.read_at, \
                n.created_at, n.updated_at \
           FROM notifications n \
           LEFT JOIN cards c ON c.id = n.card_id \
           LEFT JOIN users u ON u.id = n.actor_id \
          WHERE n.user_id = ? AND (? = 0 OR n.read_at IS NULL) \
          ORDER BY n.updated_at DESC, n.id DESC LIMIT ?",
    )
    .bind(user_id)
    .bind(unread_only)
    .bind(limit.clamp(1, 200))
    .fetch_all(db.reader())
    .await?)
}

/// How many unread notifications a user has.
pub async fn unread_count(db: &Db, user_id: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = ? AND read_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(db.reader())
    .await?)
}

/// Marks one of a user's notifications read. `false` if it is not theirs.
pub async fn mark_read(db: &Db, user_id: &str, id: &str, now: DateTime<Utc>) -> AppResult<bool> {
    let mut tx = db.begin_write().await?;
    let done = sqlx::query(
        "UPDATE notifications SET read_at = COALESCE(read_at, ?) WHERE id = ? AND user_id = ?",
    )
    .bind(to_sql_timestamp(now))
    .bind(id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(done.rows_affected() > 0)
}

/// Marks all of a user's notifications read.
pub async fn mark_all_read(db: &Db, user_id: &str, now: DateTime<Utc>) -> AppResult<u64> {
    let mut tx = db.begin_write().await?;
    let done =
        sqlx::query("UPDATE notifications SET read_at = ? WHERE user_id = ? AND read_at IS NULL")
            .bind(to_sql_timestamp(now))
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(done.rows_affected())
}

/// A user's preferences, with defaults filled in for every kind.
pub async fn preferences(db: &Db, user_id: &str) -> AppResult<Preferences> {
    let s = settings(db, user_id).await?;
    let mut channels = Vec::new();
    for kind in Kind::ALL {
        channels.push(KindChannel {
            kind,
            channel: channel_for(db, user_id, kind).await?,
        });
    }
    Ok(Preferences {
        notify_own_changes: s.notify_own_changes,
        daily_digest: s.daily_digest,
        channels,
    })
}

/// Replaces a user's preferences.
pub async fn set_preferences(db: &Db, user_id: &str, prefs: &Preferences) -> AppResult<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "INSERT INTO notification_settings (user_id, notify_own_changes, daily_digest) \
         VALUES (?, ?, ?) ON CONFLICT (user_id) DO UPDATE SET \
         notify_own_changes = excluded.notify_own_changes, daily_digest = excluded.daily_digest",
    )
    .bind(user_id)
    .bind(prefs.notify_own_changes)
    .bind(prefs.daily_digest)
    .execute(&mut *tx)
    .await?;
    for entry in &prefs.channels {
        sqlx::query(
            "INSERT INTO notification_prefs (user_id, kind, channel) VALUES (?, ?, ?) \
             ON CONFLICT (user_id, kind) DO UPDATE SET channel = excluded.channel",
        )
        .bind(user_id)
        .bind(entry.kind.as_str())
        .bind(entry.channel.as_str())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Validates a digest/email preference against whether email can be sent at all.
pub fn require_mail_for(prefs: &Preferences, mail_configured: bool) -> AppResult<()> {
    let wants_mail =
        prefs.daily_digest || prefs.channels.iter().any(|c| c.channel == Channel::Email);
    if wants_mail && !mail_configured {
        return Err(AppError::Validation(
            "Email delivery needs SMTP to be configured by an administrator first.".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_words_after_an_at_sign_not_emails_or_code() {
        assert_eq!(mentions("hey @alice and @bob."), vec!["alice", "bob"]);
        assert_eq!(mentions("@alice @ALICE @alice"), vec!["alice"]);
        assert_eq!(mentions("mail alice@example.com"), Vec::<String>::new());
        assert_eq!(mentions("`@decorator` and @carol"), vec!["carol"]);
        assert_eq!(mentions("```\n@inside\n```\n@outside"), vec!["outside"]);
        assert_eq!(mentions("(@dave)"), vec!["dave"]);
        assert_eq!(mentions("@"), Vec::<String>::new());
    }

    #[test]
    fn only_new_mentions_count_on_an_edit() {
        assert_eq!(
            new_mentions(Some("@alice"), Some("@alice and @bob")),
            vec!["bob"]
        );
        assert_eq!(new_mentions(None, Some("@alice")), vec!["alice"]);
        assert_eq!(new_mentions(Some("@alice"), None), Vec::<String>::new());
    }

    #[test]
    fn kinds_and_channels_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        for channel in [Channel::InApp, Channel::Email, Channel::Off] {
            assert_eq!(Channel::parse(channel.as_str()), Some(channel));
        }
    }

    #[test]
    fn email_preferences_need_smtp() {
        let prefs = Preferences {
            notify_own_changes: false,
            daily_digest: true,
            channels: Vec::new(),
        };
        assert!(require_mail_for(&prefs, false).is_err());
        assert!(require_mail_for(&prefs, true).is_ok());
    }
}
