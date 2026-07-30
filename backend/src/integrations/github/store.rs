//! Persistence for the GitHub integration: the `project_repos` link and the
//! `card_git_links` a card accumulates.
//!
//! One repo per project (`project_repos` is `UNIQUE(project_id)`), addressed by
//! GitHub's immutable `repo_id` plus `owner`/`repo` so a rename does not orphan the
//! link. A card's branches, PRs, and commits live in `card_git_links`, keyed
//! `(card_id, kind, ref)` so a webhook redelivery upserts rather than duplicates.
//!
//! Rows here are rows, never wire types — the API layer maps them to its own DTOs,
//! the same split [`crate::secrets`] and [`crate::domain::project`] use.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, SqliteConnection};
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

use super::RepoRef;

// ---------------------------------------------------------------------------
// project_repos
// ---------------------------------------------------------------------------

/// The columns of `project_repos` Atlas reads. The webhook-secret blobs are
/// deliberately left off — nothing outside webhook management needs them, and a
/// row is not a place to carry a sealed secret around by accident.
macro_rules! project_repo_columns {
    () => {
        "id, project_id, credential_id, owner, repo, repo_id, default_branch, \
         branch_prefix, webhook_id, created_at, updated_at"
    };
}

/// A row of `project_repos`: the one repository a project is linked to.
#[derive(Debug, Clone, FromRow)]
pub struct ProjectRepo {
    /// UUID v7. Also the AAD a per-repo webhook secret is bound to.
    pub id: String,
    /// The project this repo is linked to. `UNIQUE`, so one repo per project.
    pub project_id: String,
    /// The credential whose PAT Atlas acts with. `NULL` if that credential was
    /// later deleted — the link survives but is inert until re-pointed.
    pub credential_id: Option<String>,
    /// The repository owner (user or org login).
    pub owner: String,
    /// The repository name.
    pub repo: String,
    /// GitHub's immutable numeric id — survives a rename of `owner`/`repo`.
    pub repo_id: i64,
    /// The repo's default branch, the base new branches fork from.
    pub default_branch: String,
    /// The prefix for generated branch names (`feature/ATLAS-42-…`).
    pub branch_prefix: String,
    /// The id of the Atlas webhook on this repo, once one is installed.
    pub webhook_id: Option<i64>,
    /// When the link was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl ProjectRepo {
    /// Addresses this repo the way [`super::client::GithubClient`] wants it.
    #[must_use]
    pub fn repo_ref(&self) -> RepoRef {
        RepoRef::new(self.owner.clone(), self.repo.clone())
    }
}

/// The fields needed to link (or relink) a repo to a project.
#[derive(Debug)]
pub struct NewProjectRepo<'a> {
    /// The project being linked.
    pub project_id: &'a str,
    /// The credential whose PAT drives the link. Always set at link time; the
    /// column is nullable only because a later credential deletion sets it null.
    pub credential_id: Option<&'a str>,
    /// The repository owner.
    pub owner: &'a str,
    /// The repository name.
    pub repo: &'a str,
    /// GitHub's immutable numeric id for the repo.
    pub repo_id: i64,
    /// The repo's default branch.
    pub default_branch: &'a str,
    /// The generated-branch prefix.
    pub branch_prefix: &'a str,
}

/// Finds the repo linked to a project, if any.
pub async fn find_project_repo(db: &Db, project_id: &str) -> AppResult<Option<ProjectRepo>> {
    Ok(sqlx::query_as::<_, ProjectRepo>(concat!(
        "SELECT ",
        project_repo_columns!(),
        " FROM project_repos WHERE project_id = ?"
    ))
    .bind(project_id)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds the repo linked to a project inside an open transaction.
pub async fn find_project_repo_tx(
    tx: &mut SqliteConnection,
    project_id: &str,
) -> AppResult<Option<ProjectRepo>> {
    Ok(sqlx::query_as::<_, ProjectRepo>(concat!(
        "SELECT ",
        project_repo_columns!(),
        " FROM project_repos WHERE project_id = ?"
    ))
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Links a repo to a project, replacing any existing link (one repo per project).
pub async fn upsert_project_repo(
    tx: &mut SqliteConnection,
    new: &NewProjectRepo<'_>,
    now: DateTime<Utc>,
) -> AppResult<ProjectRepo> {
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO project_repos \
           (id, project_id, credential_id, owner, repo, repo_id, default_branch, \
            branch_prefix, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (project_id) DO UPDATE SET \
            credential_id  = excluded.credential_id, \
            owner          = excluded.owner, \
            repo           = excluded.repo, \
            repo_id        = excluded.repo_id, \
            default_branch = excluded.default_branch, \
            branch_prefix  = excluded.branch_prefix, \
            updated_at     = excluded.updated_at",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(new.project_id)
    .bind(new.credential_id)
    .bind(new.owner)
    .bind(new.repo)
    .bind(new.repo_id)
    .bind(new.default_branch)
    .bind(new.branch_prefix)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_project_repo_tx(&mut *tx, new.project_id)
        .await?
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("the project_repo just upserted is missing"))
        })
}

/// Unlinks a project's repo. Returns whether a row was actually removed.
pub async fn delete_project_repo(tx: &mut SqliteConnection, project_id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM project_repos WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ---------------------------------------------------------------------------
// card_git_links
// ---------------------------------------------------------------------------

/// The columns of `card_git_links`.
macro_rules! card_git_link_columns {
    () => {
        "id, card_id, kind, ref, url, state, meta, created_at, updated_at"
    };
}

/// A row of `card_git_links`: one branch, PR, or commit tied to a card.
#[derive(Debug, Clone, FromRow)]
pub struct CardGitLink {
    /// UUID v7.
    pub id: String,
    /// The card this git object belongs to.
    pub card_id: String,
    /// `branch` | `pr` | `commit`.
    pub kind: String,
    /// The branch name, PR number, or commit SHA. `ref` in SQL — a Rust keyword,
    /// hence the rename.
    #[sqlx(rename = "ref")]
    pub git_ref: String,
    /// The browser URL, if known.
    pub url: Option<String>,
    /// A `kind`-specific state (a PR's open/merged/closed), if known.
    pub state: Option<String>,
    /// Extra JSON metadata, if any.
    pub meta: Option<String>,
    /// When the link was first recorded.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// The fields needed to record (or refresh) a git link on a card.
#[derive(Debug)]
pub struct NewCardGitLink<'a> {
    /// The card the link belongs to.
    pub card_id: &'a str,
    /// `branch` | `pr` | `commit`.
    pub kind: &'a str,
    /// The branch name, PR number, or commit SHA.
    pub git_ref: &'a str,
    /// The browser URL, if known.
    pub url: Option<&'a str>,
    /// A `kind`-specific state, if known.
    pub state: Option<&'a str>,
    /// Extra JSON metadata, if any.
    pub meta: Option<&'a str>,
}

/// Records a git link on a card, refreshing it in place on redelivery
/// (`UNIQUE(card_id, kind, ref)`), and returns the stored row.
pub async fn upsert_card_git_link(
    tx: &mut SqliteConnection,
    new: &NewCardGitLink<'_>,
    now: DateTime<Utc>,
) -> AppResult<CardGitLink> {
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO card_git_links \
           (id, card_id, kind, ref, url, state, meta, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (card_id, kind, ref) DO UPDATE SET \
            url        = excluded.url, \
            state      = excluded.state, \
            meta       = excluded.meta, \
            updated_at = excluded.updated_at",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(new.card_id)
    .bind(new.kind)
    .bind(new.git_ref)
    .bind(new.url)
    .bind(new.state)
    .bind(new.meta)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_card_git_link_tx(&mut *tx, new.card_id, new.kind, new.git_ref)
        .await?
        .ok_or_else(|| {
            AppError::internal(anyhow::anyhow!(
                "the card_git_link just upserted is missing"
            ))
        })
}

/// Finds one git link by its `(card_id, kind, ref)` identity, inside a transaction.
async fn find_card_git_link_tx(
    tx: &mut SqliteConnection,
    card_id: &str,
    kind: &str,
    git_ref: &str,
) -> AppResult<Option<CardGitLink>> {
    Ok(sqlx::query_as::<_, CardGitLink>(concat!(
        "SELECT ",
        card_git_link_columns!(),
        " FROM card_git_links WHERE card_id = ? AND kind = ? AND ref = ?"
    ))
    .bind(card_id)
    .bind(kind)
    .bind(git_ref)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Lists every git link on a card, newest first.
pub async fn list_card_git_links(db: &Db, card_id: &str) -> AppResult<Vec<CardGitLink>> {
    Ok(sqlx::query_as::<_, CardGitLink>(concat!(
        "SELECT ",
        card_git_link_columns!(),
        " FROM card_git_links WHERE card_id = ? ORDER BY created_at DESC"
    ))
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

// ---------------------------------------------------------------------------
// card_worklogs
// ---------------------------------------------------------------------------

/// The fields needed to record a worklog against a card.
#[derive(Debug)]
pub struct NewWorklog<'a> {
    /// The card the time is logged against.
    pub card_id: &'a str,
    /// Who did the work, if known. `NULL` survives that account's later deletion.
    pub author_id: Option<&'a str>,
    /// Minutes worked — must be positive (the column is `CHECK (minutes > 0)`).
    pub minutes: i64,
    /// An optional note (the words trailing a `#time 2h` directive).
    pub note: Option<&'a str>,
    /// Where the log came from, e.g. `smart-commit`.
    pub source: &'a str,
}

/// Appends a worklog to a card. `card_worklogs` is append-only — no `updated_at`.
///
/// A non-positive duration is rejected here rather than left to the DB's
/// `CHECK (minutes > 0)`, so the caller gets a clear error instead of an opaque 500.
pub async fn insert_worklog(
    tx: &mut SqliteConnection,
    new: &NewWorklog<'_>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    if new.minutes <= 0 {
        return Err(AppError::Validation(
            "a worklog must be a positive number of minutes".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO card_worklogs (id, card_id, author_id, minutes, note, source, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(new.card_id)
    .bind(new.author_id)
    .bind(new.minutes)
    .bind(new.note)
    .bind(new.source)
    .bind(to_sql_timestamp(now))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;
    use crate::domain::EstimationUnit;
    use crate::domain::project::{self, NewProject};
    use crate::test_support::TempDb;

    async fn db() -> (Db, TempDb) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        (db, temp)
    }

    /// Seeds a bare project row and returns its id — enough to satisfy the
    /// `project_repos.project_id` foreign key.
    async fn a_project(db: &Db, key: &str) -> String {
        let mut tx = db.begin_write().await.unwrap();
        let project = project::insert(
            &mut tx,
            &NewProject {
                key: key.to_owned(),
                name: key.to_owned(),
                description: None,
                lead_id: None,
                template: "blank".to_owned(),
                cycles_enabled: false,
                estimation_unit: EstimationUnit::None,
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        project.id
    }

    #[tokio::test]
    async fn a_repo_link_round_trips_and_a_project_keeps_exactly_one_repo() {
        let (db, _temp) = db().await;
        let project_id = a_project(&db, "ATLAS").await;

        // Link.
        let mut tx = db.begin_write().await.unwrap();
        let linked = upsert_project_repo(
            &mut tx,
            &NewProjectRepo {
                project_id: &project_id,
                credential_id: None,
                owner: "octocat",
                repo: "hello",
                repo_id: 42,
                default_branch: "main",
                branch_prefix: "feature",
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(linked.repo_ref(), RepoRef::new("octocat", "hello"));
        assert_eq!(linked.repo_id, 42);

        let found = find_project_repo(&db, &project_id).await.unwrap().unwrap();
        assert_eq!(found.id, linked.id);
        assert_eq!(found.default_branch, "main");

        // Relinking replaces the row in place rather than adding a second: the
        // `UNIQUE(project_id)` upsert keeps the same id and rewrites the fields.
        let mut tx = db.begin_write().await.unwrap();
        let relinked = upsert_project_repo(
            &mut tx,
            &NewProjectRepo {
                project_id: &project_id,
                credential_id: None,
                owner: "octocat",
                repo: "world",
                repo_id: 99,
                default_branch: "trunk",
                branch_prefix: "wip",
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(relinked.id, linked.id, "the same row is updated in place");
        assert_eq!(relinked.repo, "world");
        assert_eq!(relinked.repo_id, 99);
        assert_eq!(relinked.branch_prefix, "wip");

        // Unlink, and unlinking again reports nothing removed.
        let mut tx = db.begin_write().await.unwrap();
        assert!(delete_project_repo(&mut tx, &project_id).await.unwrap());
        tx.commit().await.unwrap();
        assert!(find_project_repo(&db, &project_id).await.unwrap().is_none());

        let mut tx = db.begin_write().await.unwrap();
        assert!(!delete_project_repo(&mut tx, &project_id).await.unwrap());
        tx.commit().await.unwrap();
    }
}
