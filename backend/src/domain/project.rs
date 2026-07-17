//! The `projects` row, its API representation, and the atomic card-key counter.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::EstimationUnit;
use crate::error::{AppError, AppResult};

/// Longest accepted project key, in characters.
///
/// Jira's limit is 10. Every card key carries this prefix and every board
/// renders it, so it is a display budget as much as a storage one.
pub const MAX_KEY: usize = 10;

/// Longest accepted project name, in characters.
pub const MAX_NAME: usize = 128;

/// Longest accepted project description, in characters.
pub const MAX_DESCRIPTION: usize = 16 * 1024;

/// A row of `projects`, exactly as stored.
///
/// Not `Serialize`, matching [`crate::auth::User`]. Nothing here is secret today
/// — but the convention is that a row is a row and a DTO is what goes on the
/// wire, so that the day a column *is* sensitive, nobody has to notice.
#[derive(Debug, Clone, FromRow)]
pub struct Project {
    /// UUID v7, as text.
    pub id: String,
    /// The card-key prefix: `ATLAS` in `ATLAS-123`. Uppercase.
    pub key: String,
    /// The human name.
    pub name: String,
    /// Optional markdown description.
    pub description: Option<String>,
    /// The project lead's user id.
    pub lead_id: Option<String>,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// Optional cover image URL — Phase 14 generates these.
    pub cover_image_url: Option<String>,
    /// Which template seeded this project. Descriptive, not behavioural.
    pub template: String,
    /// The highest card number allocated so far. Never rewinds.
    pub card_counter: i64,
    /// Whether cycles are on for this project.
    pub cycles_enabled: bool,
    /// How this project's `estimate` field is interpreted.
    pub estimation_unit: EstimationUnit,
    /// When the project was archived. `None` = live.
    pub archived_at: Option<DateTime<Utc>>,
    /// When the project was created.
    pub created_at: DateTime<Utc>,
    /// When the project last changed.
    pub updated_at: DateTime<Utc>,
}

impl Project {
    /// Whether the project is archived.
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

/// A project as the API describes it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    /// UUID v7, as text.
    pub id: String,
    /// The card-key prefix.
    #[schema(example = "ATLAS")]
    pub key: String,
    /// The human name.
    pub name: String,
    /// Optional markdown description.
    pub description: Option<String>,
    /// The project lead's user id.
    pub lead_id: Option<String>,
    /// Optional avatar URL.
    pub avatar_url: Option<String>,
    /// Optional cover image URL.
    pub cover_image_url: Option<String>,
    /// Which template seeded this project.
    #[schema(example = "programming")]
    pub template: String,
    /// The highest card number allocated so far.
    pub card_counter: i64,
    /// Whether cycles are on.
    pub cycles_enabled: bool,
    /// How the `estimate` field is interpreted.
    pub estimation_unit: EstimationUnit,
    /// When the project was archived, if it is.
    pub archived_at: Option<DateTime<Utc>>,
    /// When the project was created.
    pub created_at: DateTime<Utc>,
    /// When the project last changed.
    pub updated_at: DateTime<Utc>,
}

impl From<&Project> for ProjectDto {
    fn from(project: &Project) -> Self {
        Self {
            id: project.id.clone(),
            key: project.key.clone(),
            name: project.name.clone(),
            description: project.description.clone(),
            lead_id: project.lead_id.clone(),
            avatar_url: project.avatar_url.clone(),
            cover_image_url: project.cover_image_url.clone(),
            template: project.template.clone(),
            card_counter: project.card_counter,
            cycles_enabled: project.cycles_enabled,
            estimation_unit: project.estimation_unit,
            archived_at: project.archived_at,
            created_at: project.created_at,
            updated_at: project.updated_at,
        }
    }
}

impl From<Project> for ProjectDto {
    fn from(project: Project) -> Self {
        Self::from(&project)
    }
}

/// Every column of `projects`.
///
/// A macro rather than a `const` so `concat!` can splice it — `concat!` takes
/// literals only. The payoff is that every query below is a `&'static str` and
/// satisfies sqlx 0.9's `SqlSafeStr` bound without `AssertSqlSafe`. Lifted
/// verbatim from `auth::user`'s `user_columns!`.
macro_rules! project_columns {
    () => {
        "id, key, name, description, lead_id, avatar_url, cover_image_url, template, \
         card_counter, cycles_enabled, estimation_unit, archived_at, created_at, updated_at"
    };
}

/// A new project, ready to insert.
#[derive(Debug)]
pub struct NewProject {
    /// The card-key prefix. Uppercased and validated by [`validate_key`].
    pub key: String,
    /// The human name.
    pub name: String,
    /// Optional markdown description.
    pub description: Option<String>,
    /// The project lead's user id.
    pub lead_id: Option<String>,
    /// Which template is seeding this project.
    pub template: String,
    /// Whether cycles are on.
    pub cycles_enabled: bool,
    /// How the `estimate` field is interpreted.
    pub estimation_unit: EstimationUnit,
}

/// Normalises and checks a project key.
///
/// Keys are `[A-Z][A-Z0-9]*`, uppercased on the way in. The rules are not
/// arbitrary: the key is the prefix of every card key, `ATLAS-123` is parsed by
/// splitting on the **last** `-`, and Phase 12's smart commits and Phase 9's
/// autolinker both scan free text for the pattern. A key containing a digit
/// first, a `-`, or a space would make that scan ambiguous or wrong.
pub fn validate_key(key: &str) -> AppResult<String> {
    let key = key.trim().to_ascii_uppercase();

    if key.is_empty() {
        return Err(AppError::Validation(
            "Project key must not be empty.".to_owned(),
        ));
    }
    if key.chars().count() > MAX_KEY {
        return Err(AppError::Validation(format!(
            "Project key must be at most {MAX_KEY} characters long."
        )));
    }
    if !key.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(AppError::Validation(
            "Project key must start with a letter.".to_owned(),
        ));
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::Validation(
            "Project key must contain only letters and digits — no spaces, hyphens or \
             punctuation. It becomes the prefix of every card key."
                .to_owned(),
        ));
    }

    Ok(key)
}

/// Checks a project name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();

    if name.is_empty() {
        return Err(AppError::Validation(
            "Project name must not be empty.".to_owned(),
        ));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "Project name must be at most {MAX_NAME} characters long."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Project name must not contain control characters.".to_owned(),
        ));
    }

    Ok(name.to_owned())
}

/// Checks a description. Markdown, so only the length is bounded.
pub fn validate_description(description: &str) -> AppResult<String> {
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(AppError::Validation(format!(
            "Description must be at most {MAX_DESCRIPTION} characters long."
        )));
    }
    Ok(description.to_owned())
}

/// Inserts a project and returns it.
///
/// Takes a transaction, not the pool: creating a project also seeds its levels,
/// types, statuses, priorities and resolutions, and a project that exists with
/// no statuses is a project no card can be created in. Either all of it lands or
/// none of it does.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    new: &NewProject,
    now: DateTime<Utc>,
) -> AppResult<Project> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);

    sqlx::query(
        "INSERT INTO projects (id, key, name, description, lead_id, avatar_url, \
         cover_image_url, template, card_counter, cycles_enabled, estimation_unit, \
         archived_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, 0, ?, ?, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&new.key)
    .bind(&new.name)
    .bind(&new.description)
    .bind(&new.lead_id)
    .bind(&new.template)
    .bind(new.cycles_enabled)
    .bind(new.estimation_unit)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the project just inserted is missing")))
}

/// Finds a project by key, case-insensitively.
///
/// The case-insensitivity is the column's `COLLATE NOCASE`, so `atlas` finds
/// `ATLAS` and the lookup still uses the index.
pub async fn find_by_key(db: &Db, key: &str) -> AppResult<Option<Project>> {
    Ok(sqlx::query_as::<_, Project>(concat!(
        "SELECT ",
        project_columns!(),
        " FROM projects WHERE key = ?"
    ))
    .bind(key)
    .fetch_optional(db.reader())
    .await?)
}

/// Finds a project by key inside an open transaction.
pub async fn find_by_key_tx(
    tx: &mut sqlx::SqliteConnection,
    key: &str,
) -> AppResult<Option<Project>> {
    Ok(sqlx::query_as::<_, Project>(concat!(
        "SELECT ",
        project_columns!(),
        " FROM projects WHERE key = ?"
    ))
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Finds a project by id inside an open transaction.
pub async fn find_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Project>> {
    Ok(sqlx::query_as::<_, Project>(concat!(
        "SELECT ",
        project_columns!(),
        " FROM projects WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Every project, newest first, optionally including archived ones.
pub async fn list(db: &Db, include_archived: bool) -> AppResult<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(concat!(
        "SELECT ",
        project_columns!(),
        " FROM projects WHERE (? OR archived_at IS NULL) ORDER BY name, key"
    ))
    .bind(include_archived)
    .fetch_all(db.reader())
    .await?)
}

/// Whether a project key is taken, case-insensitively.
pub async fn key_taken(
    tx: &mut sqlx::SqliteConnection,
    key: &str,
    excluding_id: Option<&str>,
) -> AppResult<bool> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM projects WHERE key = ? AND (? IS NULL OR id != ?))",
    )
    .bind(key)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(taken)
}

/// The fields a `PATCH /projects/{key}` may change.
///
/// `Option<Option<T>>` on the nullable fields keeps absent (leave alone) and
/// `null` (clear) distinct — the same pattern as [`crate::auth::user::UserPatch`].
///
/// `key` is deliberately absent: renaming a project key would invalidate every
/// card key under it, which is a bulk move, not a field edit.
#[allow(clippy::option_option)]
#[derive(Debug, Default)]
pub struct ProjectPatch {
    /// The name, if it is changing.
    pub name: Option<String>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub description: Option<Option<String>>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub lead_id: Option<Option<String>>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub avatar_url: Option<Option<String>>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub cover_image_url: Option<Option<String>>,
    /// Whether cycles are on, if it is changing.
    pub cycles_enabled: Option<bool>,
    /// The estimation unit, if it is changing.
    pub estimation_unit: Option<EstimationUnit>,
}

impl ProjectPatch {
    /// Whether this patch would change anything.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.lead_id.is_none()
            && self.avatar_url.is_none()
            && self.cover_image_url.is_none()
            && self.cycles_enabled.is_none()
            && self.estimation_unit.is_none()
    }
}

/// Applies a patch.
///
/// One fixed statement writes every column, exactly as
/// [`crate::auth::user::apply_patch`] does: `COALESCE(?, column)` means "leave
/// it alone when the parameter is NULL", and the nullable columns — where NULL
/// is a value the caller may legitimately mean — get a
/// `CASE WHEN <should-write> THEN ? ELSE column END` and an explicit flag.
///
/// The alternative, building a `SET` list from whichever fields are present,
/// assembles SQL from a runtime shape. That is the habit that produces injection
/// bugs even where this particular instance would have been safe.
pub async fn apply_patch(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    patch: &ProjectPatch,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE projects SET \
           name            = COALESCE(?, name), \
           description     = CASE WHEN ? THEN ? ELSE description END, \
           lead_id         = CASE WHEN ? THEN ? ELSE lead_id END, \
           avatar_url      = CASE WHEN ? THEN ? ELSE avatar_url END, \
           cover_image_url = CASE WHEN ? THEN ? ELSE cover_image_url END, \
           cycles_enabled  = COALESCE(?, cycles_enabled), \
           estimation_unit = COALESCE(?, estimation_unit), \
           updated_at      = ? \
         WHERE id = ?",
    )
    .bind(patch.name.clone())
    .bind(patch.description.is_some())
    .bind(patch.description.clone().flatten())
    .bind(patch.lead_id.is_some())
    .bind(patch.lead_id.clone().flatten())
    .bind(patch.avatar_url.is_some())
    .bind(patch.avatar_url.clone().flatten())
    .bind(patch.cover_image_url.is_some())
    .bind(patch.cover_image_url.clone().flatten())
    .bind(patch.cycles_enabled)
    .bind(patch.estimation_unit)
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Archives or unarchives a project.
pub async fn set_archived(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    archived: bool,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let timestamp = to_sql_timestamp(now);
    sqlx::query("UPDATE projects SET archived_at = ?, updated_at = ? WHERE id = ?")
        .bind(archived.then_some(timestamp.clone()))
        .bind(&timestamp)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Hard-deletes a project and everything under it.
///
/// The only hard delete in Atlas, and it is deliberate: archive is the reversible
/// answer, and this one is "I created this by mistake, take it away". Every child
/// table cascades from `projects`, which is why `cards`' config foreign keys are
/// `DEFERRABLE INITIALLY DEFERRED` — see migration 0003.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Allocates the next card key for a project: `ATLAS-7`.
///
/// # Why this is one statement
///
/// `UPDATE ... SET card_counter = card_counter + 1 ... RETURNING card_counter`
/// reads and writes the counter atomically inside the caller's write
/// transaction. The obvious alternative — `SELECT card_counter`, add one,
/// `UPDATE` — has a window between the read and the write, and two concurrent
/// creates that both read 6 both write 7 and both try to insert `ATLAS-7`. One
/// gets a card; the other gets a `UNIQUE` violation surfaced as a 500 at the end
/// of a form the user just filled in.
///
/// Three things guard this, and all three are wanted:
///
/// 1. this statement, which cannot interleave with itself;
/// 2. `Db::begin_write`'s `BEGIN IMMEDIATE`, which holds the write lock from the
///    start of the transaction, so the counter cannot move under a caller that
///    goes on to do more work;
/// 3. `cards.key`'s `UNIQUE` index, the backstop that turns any future mistake
///    into a loud failure rather than two cards with one key.
///
/// The counter is **never decremented**, so a deleted card burns its number. That
/// is the point: reusing `ATLAS-7` would silently repoint every bookmark, commit
/// message and comment that ever referenced the original.
pub async fn allocate_card_key(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
) -> AppResult<String> {
    let key: Option<(String, i64)> = sqlx::query_as(
        "UPDATE projects SET card_counter = card_counter + 1 \
         WHERE id = ? \
         RETURNING key, card_counter",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (project_key, counter) = key.ok_or(AppError::NotFound)?;
    Ok(format!("{project_key}-{counter}"))
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

    async fn insert_project(db: &Db, key: &str) -> Project {
        let mut tx = db.begin_write().await.unwrap();
        let project = insert(
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
        project
    }

    #[test]
    fn keys_are_uppercased_and_constrained_to_the_autolinkable_shape() {
        assert_eq!(validate_key(" atlas ").unwrap(), "ATLAS");
        assert_eq!(validate_key("Atlas2").unwrap(), "ATLAS2");

        assert!(validate_key("").is_err());
        assert!(validate_key(&"A".repeat(MAX_KEY + 1)).is_err());
        assert!(validate_key(&"A".repeat(MAX_KEY)).is_ok());

        // `ATLAS-123` is parsed by splitting on the last `-`. A key containing a
        // hyphen, a space, or leading digits makes that parse ambiguous — and
        // Phase 12's smart-commit scanner reads the same shape out of free text.
        assert!(validate_key("MY-PROJ").is_err());
        assert!(validate_key("MY PROJ").is_err());
        assert!(validate_key("1ST").is_err());
        assert!(validate_key("PROJ!").is_err());
        assert!(validate_key("PRO_J").is_err());
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("  Atlas  ").unwrap(), "Atlas");
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("Atlas\nInjected").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
    }

    #[tokio::test]
    async fn a_project_round_trips_through_the_database() {
        let (db, _temp) = db().await;
        let created = insert_project(&db, "ATLAS").await;

        let found = find_by_key(&db, "ATLAS").await.unwrap().unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.key, "ATLAS");
        assert_eq!(found.card_counter, 0);
        assert!(!found.is_archived());
        assert_eq!(found.estimation_unit, EstimationUnit::None);
        assert_eq!(found.created_at, created.created_at);

        db.close().await;
    }

    #[tokio::test]
    async fn project_keys_are_unique_case_insensitively() {
        // The column's COLLATE NOCASE. Two projects called ATLAS and atlas would
        // make `ATLAS-1` ambiguous, which is unrecoverable.
        let (db, _temp) = db().await;
        insert_project(&db, "ATLAS").await;

        assert!(find_by_key(&db, "atlas").await.unwrap().is_some());

        let mut tx = db.begin_write().await.unwrap();
        assert!(key_taken(&mut tx, "atlas", None).await.unwrap());
        assert!(!key_taken(&mut tx, "OTHER", None).await.unwrap());

        let clash = insert(
            &mut tx,
            &NewProject {
                key: "atlas".to_owned(),
                name: "Clash".to_owned(),
                description: None,
                lead_id: None,
                template: "blank".to_owned(),
                cycles_enabled: false,
                estimation_unit: EstimationUnit::None,
            },
            crate::auth::now(),
        )
        .await;
        assert!(
            clash.is_err(),
            "the UNIQUE index must reject a case variant"
        );
        tx.rollback().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn keys_are_allocated_in_sequence_and_never_reused() {
        let (db, _temp) = db().await;
        let project = insert_project(&db, "ATLAS").await;

        let mut tx = db.begin_write().await.unwrap();
        for expected in 1..=5 {
            let key = allocate_card_key(&mut tx, &project.id).await.unwrap();
            assert_eq!(key, format!("ATLAS-{expected}"));
        }
        tx.commit().await.unwrap();

        // A rolled-back allocation still burns the number in the next successful
        // transaction? No — the counter rolls back with it. What must never
        // happen is a *committed* allocation being handed out twice.
        let mut tx = db.begin_write().await.unwrap();
        assert_eq!(
            allocate_card_key(&mut tx, &project.id).await.unwrap(),
            "ATLAS-6"
        );
        tx.commit().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn allocating_a_key_for_a_project_that_does_not_exist_is_a_404() {
        let (db, _temp) = db().await;
        let mut tx = db.begin_write().await.unwrap();
        let err = allocate_card_key(&mut tx, "nope").await.unwrap_err();
        assert!(matches!(err, AppError::NotFound));
        tx.rollback().await.unwrap();
        db.close().await;
    }

    #[tokio::test]
    async fn archiving_hides_a_project_from_the_default_listing() {
        let (db, _temp) = db().await;
        let live = insert_project(&db, "LIVE").await;
        let gone = insert_project(&db, "GONE").await;

        let mut tx = db.begin_write().await.unwrap();
        set_archived(&mut tx, &gone.id, true, crate::auth::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let keys: Vec<String> = list(&db, false)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, ["LIVE"]);

        let keys: Vec<String> = list(&db, true)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, ["GONE", "LIVE"]);

        // ...and it comes back.
        let mut tx = db.begin_write().await.unwrap();
        set_archived(&mut tx, &gone.id, false, crate::auth::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(list(&db, false).await.unwrap().len(), 2);

        let _ = live;
        db.close().await;
    }

    #[tokio::test]
    async fn a_patch_leaves_absent_fields_alone_and_clears_explicit_nulls() {
        let (db, _temp) = db().await;
        let project = insert_project(&db, "ATLAS").await;

        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &project.id,
            &ProjectPatch {
                description: Some(Some("A board".to_owned())),
                ..ProjectPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // An absent description must not disturb it while a present name does.
        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &project.id,
            &ProjectPatch {
                name: Some("Renamed".to_owned()),
                ..ProjectPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let after = find_by_key(&db, "ATLAS").await.unwrap().unwrap();
        assert_eq!(after.name, "Renamed");
        assert_eq!(
            after.description,
            Some("A board".to_owned()),
            "absent != null"
        );

        // An explicit null clears it — the case a plain Option cannot express.
        let mut tx = db.begin_write().await.unwrap();
        apply_patch(
            &mut tx,
            &project.id,
            &ProjectPatch {
                description: Some(None),
                ..ProjectPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            find_by_key(&db, "ATLAS")
                .await
                .unwrap()
                .unwrap()
                .description,
            None
        );

        db.close().await;
    }

    #[tokio::test]
    async fn the_database_rejects_an_estimation_unit_outside_the_six() {
        let (db, _temp) = db().await;
        let err = sqlx::query(
            "INSERT INTO projects (id, key, name, template, card_counter, cycles_enabled, \
             estimation_unit, created_at, updated_at) \
             VALUES ('x', 'X', 'X', 'blank', 0, 0, 'story-points', 'now', 'now')",
        )
        .execute(db.writer())
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");
        db.close().await;
    }
}
