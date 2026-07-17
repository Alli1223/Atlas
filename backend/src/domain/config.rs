//! Per-project configuration: hierarchy levels, card types, statuses,
//! priorities, resolutions.
//!
//! # Why these are five small tables and not a scheme layer
//!
//! Jira routes every one of these through a three-level indirection — a status
//! belongs to a Workflow, which belongs to a Workflow Scheme, which is assigned
//! to a Project — and does it six times over (Screens, Field Configurations,
//! Issue Types, Notifications, Permissions, Workflows). That machinery exists to
//! share config across hundreds of projects, and it is the single largest source
//! of Jira's "why can't I just change this" misery.
//!
//! Atlas has one indirection: `project_id`. Copying config between projects is a
//! Phase 18 action ("copy config from project X"), which is a loop over these
//! tables and costs nothing to build. See docs/adr/0003.

use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::db::Db;
use crate::domain::StatusCategory;
use crate::error::{AppError, AppResult};

/// Longest accepted name for any config row, in characters.
pub const MAX_NAME: usize = 64;

/// Checks a config row's name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();

    if name.is_empty() {
        return Err(AppError::Validation("Name must not be empty.".to_owned()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "Name must be at most {MAX_NAME} characters long."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Name must not contain control characters.".to_owned(),
        ));
    }

    Ok(name.to_owned())
}

// ---------------------------------------------------------------------------
// hierarchy_levels
// ---------------------------------------------------------------------------

/// A rung of one project's hierarchy.
///
/// The table that makes one engine serve unrelated domains. Nothing in Atlas
/// knows the word "Epic"; a project's levels are data. See docs/adr/0002.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HierarchyLevel {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// Higher is further up the tree. May be negative; need not be contiguous.
    pub level: i64,
    /// What this project calls the level: `Epic`, `Asset`, `Company`.
    pub name: String,
}

/// Every hierarchy level of a project, deepest last.
pub async fn levels(db: &Db, project_id: &str) -> AppResult<Vec<HierarchyLevel>> {
    Ok(sqlx::query_as::<_, HierarchyLevel>(
        "SELECT id, project_id, level, name FROM hierarchy_levels \
         WHERE project_id = ? ORDER BY level DESC",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Inserts a hierarchy level.
pub async fn insert_level(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    level: i64,
    name: &str,
) -> AppResult<HierarchyLevel> {
    let id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO hierarchy_levels (id, project_id, level, name) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(project_id)
        .bind(level)
        .bind(name)
        .execute(&mut *tx)
        .await?;

    Ok(HierarchyLevel {
        id,
        project_id: project_id.to_owned(),
        level,
        name: name.to_owned(),
    })
}

/// Renames a hierarchy level.
///
/// The level number itself is immutable through this path: changing it would
/// re-home every card type on that rung and silently reshape the tree. Deleting
/// and re-adding is the honest way to do that, and the composite foreign key
/// from `card_types` will refuse until the types have moved.
pub async fn rename_level(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: &str,
) -> AppResult<HierarchyLevel> {
    sqlx::query("UPDATE hierarchy_levels SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query_as::<_, HierarchyLevel>(
        "SELECT id, project_id, level, name FROM hierarchy_levels WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// card_types
// ---------------------------------------------------------------------------

/// A kind of card, and the hierarchy rung it sits on.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardType {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// `Story`, `Asset`, `Application`.
    pub name: String,
    /// An icon identifier the frontend resolves. Lucide names, in practice.
    pub icon: Option<String>,
    /// A hex colour.
    pub colour: Option<String>,
    /// Which hierarchy rung. Must be a level the project has defined — the
    /// composite foreign key in migration 0003 enforces it.
    pub level: i64,
    /// Whether new cards default to this type.
    pub is_default: bool,
}

/// Every card type of a project.
pub async fn card_types(db: &Db, project_id: &str) -> AppResult<Vec<CardType>> {
    Ok(sqlx::query_as::<_, CardType>(
        "SELECT id, project_id, name, icon, colour, level, is_default FROM card_types \
         WHERE project_id = ? ORDER BY level DESC, name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a card type by id, scoped to a project.
///
/// The project scoping is not decoration: without it a caller could hand a card
/// the type of a *different* project, which the `cards.type_id` foreign key
/// would happily accept — the FK says "a card type", not "a card type of this
/// project". Every lookup here is scoped for that reason.
pub async fn find_card_type_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    id: &str,
) -> AppResult<Option<CardType>> {
    Ok(sqlx::query_as::<_, CardType>(
        "SELECT id, project_id, name, icon, colour, level, is_default FROM card_types \
         WHERE project_id = ? AND id = ?",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A project's default card type: the flagged one, else the lowest-level one.
pub async fn default_card_type_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
) -> AppResult<Option<CardType>> {
    Ok(sqlx::query_as::<_, CardType>(
        "SELECT id, project_id, name, icon, colour, level, is_default FROM card_types \
         WHERE project_id = ? ORDER BY is_default DESC, level DESC, name LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// The default card type at a given level, else any type at that level.
///
/// Used when a card moves between projects and needs a type on the same rung.
pub async fn card_type_at_level_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    level: i64,
) -> AppResult<Option<CardType>> {
    Ok(sqlx::query_as::<_, CardType>(
        "SELECT id, project_id, name, icon, colour, level, is_default FROM card_types \
         WHERE project_id = ? AND level = ? ORDER BY is_default DESC, name LIMIT 1",
    )
    .bind(project_id)
    .bind(level)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a card type.
pub async fn insert_card_type(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    icon: Option<&str>,
    colour: Option<&str>,
    level: i64,
    is_default: bool,
) -> AppResult<CardType> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO card_types (id, project_id, name, icon, colour, level, is_default) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(icon)
    .bind(colour)
    .bind(level)
    .bind(is_default)
    .execute(&mut *tx)
    .await?;

    Ok(CardType {
        id,
        project_id: project_id.to_owned(),
        name: name.to_owned(),
        icon: icon.map(ToOwned::to_owned),
        colour: colour.map(ToOwned::to_owned),
        level,
        is_default,
    })
}

/// Edits a card type's presentation.
///
/// `level` is not editable here for the same reason `hierarchy_levels.level` is
/// not: moving a type between rungs would silently invalidate the
/// `parent.level > child.level` invariant for every existing card of that type,
/// with no way to tell the user which cards just became illegal.
pub async fn update_card_type(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: Option<&str>,
    icon: Option<Option<&str>>,
    colour: Option<Option<&str>>,
    is_default: Option<bool>,
) -> AppResult<CardType> {
    sqlx::query(
        "UPDATE card_types SET \
           name       = COALESCE(?, name), \
           icon       = CASE WHEN ? THEN ? ELSE icon END, \
           colour     = CASE WHEN ? THEN ? ELSE colour END, \
           is_default = COALESCE(?, is_default) \
         WHERE id = ?",
    )
    .bind(name)
    .bind(icon.is_some())
    .bind(icon.flatten())
    .bind(colour.is_some())
    .bind(colour.flatten())
    .bind(is_default)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query_as::<_, CardType>(
        "SELECT id, project_id, name, icon, colour, level, is_default FROM card_types WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// statuses
// ---------------------------------------------------------------------------

/// A column on a project's board.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// `In Review`, `Phone Screen`, `Retopo`.
    pub name: String,
    /// Which of the three buckets this status falls into.
    pub category: StatusCategory,
    /// Board order, left to right.
    pub position: i64,
}

/// Every status of a project, in board order.
pub async fn statuses(db: &Db, project_id: &str) -> AppResult<Vec<Status>> {
    Ok(sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses \
         WHERE project_id = ? ORDER BY position, name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a status by id, scoped to a project.
pub async fn find_status_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    id: &str,
) -> AppResult<Option<Status>> {
    Ok(sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses \
         WHERE project_id = ? AND id = ?",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Reads a status by id alone, without knowing its project.
pub async fn status_by_id_tx(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
) -> AppResult<Option<Status>> {
    Ok(sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A project's first status: where a new card lands.
pub async fn first_status_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
) -> AppResult<Option<Status>> {
    Ok(sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses \
         WHERE project_id = ? ORDER BY position, name LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// The first status of a project in a given category.
///
/// The mapping used when a card moves between projects: statuses do not survive
/// the move, but the *category* does, so a card that was in progress stays in
/// progress rather than silently reopening.
pub async fn first_status_in_category_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    category: StatusCategory,
) -> AppResult<Option<Status>> {
    Ok(sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses \
         WHERE project_id = ? AND category = ? ORDER BY position, name LIMIT 1",
    )
    .bind(project_id)
    .bind(category)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a status.
pub async fn insert_status(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    category: StatusCategory,
    position: i64,
) -> AppResult<Status> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO statuses (id, project_id, name, category, position) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(category)
    .bind(position)
    .execute(&mut *tx)
    .await?;

    Ok(Status {
        id,
        project_id: project_id.to_owned(),
        name: name.to_owned(),
        category,
        position,
    })
}

/// Edits a status.
///
/// The category **is** editable, and that is a real decision with a real
/// consequence: flipping a status into or out of `done` changes what every
/// existing card in it means. Atlas does not retroactively rewrite those cards'
/// resolutions — history says what happened, and rewriting it would be a lie.
/// The next transition through the status applies the rule.
pub async fn update_status(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: Option<&str>,
    category: Option<StatusCategory>,
    position: Option<i64>,
) -> AppResult<Status> {
    sqlx::query(
        "UPDATE statuses SET \
           name     = COALESCE(?, name), \
           category = COALESCE(?, category), \
           position = COALESCE(?, position) \
         WHERE id = ?",
    )
    .bind(name)
    .bind(category)
    .bind(position)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query_as::<_, Status>(
        "SELECT id, project_id, name, category, position FROM statuses WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// priorities
// ---------------------------------------------------------------------------

/// How urgent a card is. Ordered.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Priority {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// `Highest`, `Dream Job`, `Critical`.
    pub name: String,
    /// An icon identifier.
    pub icon: Option<String>,
    /// A hex colour.
    pub colour: Option<String>,
    /// **Lower is more urgent.** Rank 1 is the most urgent priority.
    ///
    /// This ordinal is why `priorities` has a rank at all: `priority > High` is
    /// a query Atlas has to answer (Phase 6), and it is meaningless over a set
    /// of names. Note this is an INTEGER ordinal and has nothing to do with
    /// [`crate::rank::Rank`], the card's lexicographic sort key.
    pub rank: i64,
}

/// Every priority of a project, most urgent first.
pub async fn priorities(db: &Db, project_id: &str) -> AppResult<Vec<Priority>> {
    Ok(sqlx::query_as::<_, Priority>(
        "SELECT id, project_id, name, icon, colour, rank FROM priorities \
         WHERE project_id = ? ORDER BY rank, name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a priority by id, scoped to a project.
pub async fn find_priority_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    id: &str,
) -> AppResult<Option<Priority>> {
    Ok(sqlx::query_as::<_, Priority>(
        "SELECT id, project_id, name, icon, colour, rank FROM priorities \
         WHERE project_id = ? AND id = ?",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A priority of a project with a given name.
pub async fn priority_by_name_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
) -> AppResult<Option<Priority>> {
    Ok(sqlx::query_as::<_, Priority>(
        "SELECT id, project_id, name, icon, colour, rank FROM priorities \
         WHERE project_id = ? AND name = ?",
    )
    .bind(project_id)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a priority.
pub async fn insert_priority(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    icon: Option<&str>,
    colour: Option<&str>,
    rank: i64,
) -> AppResult<Priority> {
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO priorities (id, project_id, name, icon, colour, rank) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(icon)
    .bind(colour)
    .bind(rank)
    .execute(&mut *tx)
    .await?;

    Ok(Priority {
        id,
        project_id: project_id.to_owned(),
        name: name.to_owned(),
        icon: icon.map(ToOwned::to_owned),
        colour: colour.map(ToOwned::to_owned),
        rank,
    })
}

/// Edits a priority.
pub async fn update_priority(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: Option<&str>,
    icon: Option<Option<&str>>,
    colour: Option<Option<&str>>,
    rank: Option<i64>,
) -> AppResult<Priority> {
    sqlx::query(
        "UPDATE priorities SET \
           name   = COALESCE(?, name), \
           icon   = CASE WHEN ? THEN ? ELSE icon END, \
           colour = CASE WHEN ? THEN ? ELSE colour END, \
           rank   = COALESCE(?, rank) \
         WHERE id = ?",
    )
    .bind(name)
    .bind(icon.is_some())
    .bind(icon.flatten())
    .bind(colour.is_some())
    .bind(colour.flatten())
    .bind(rank)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query_as::<_, Priority>(
        "SELECT id, project_id, name, icon, colour, rank FROM priorities WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
}

// ---------------------------------------------------------------------------
// resolutions
// ---------------------------------------------------------------------------

/// Why a card stopped.
///
/// A card is resolved **iff** `resolution_id IS NOT NULL`, independently of its
/// status — that is Jira's model and it is genuinely the right one, because
/// "reached the Done column" and "was actually finished" are different facts.
/// What Atlas fixes is the *ergonomics*: the two are kept in sync automatically
/// by [`crate::domain::card::update`] rather than left to a workflow
/// post-function that anyone can forget to add. See docs/adr §E.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project.
    pub project_id: String,
    /// `Done`, `Won't Do`, `Ghosted`.
    pub name: String,
    /// Display order. Position 1 is the default a done-transition auto-sets.
    pub position: i64,
}

/// Every resolution of a project.
pub async fn resolutions(db: &Db, project_id: &str) -> AppResult<Vec<Resolution>> {
    Ok(sqlx::query_as::<_, Resolution>(
        "SELECT id, project_id, name, position FROM resolutions \
         WHERE project_id = ? ORDER BY position, name",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Finds a resolution by id, scoped to a project.
pub async fn find_resolution_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    id: &str,
) -> AppResult<Option<Resolution>> {
    Ok(sqlx::query_as::<_, Resolution>(
        "SELECT id, project_id, name, position FROM resolutions \
         WHERE project_id = ? AND id = ?",
    )
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A resolution of a project with a given name.
pub async fn resolution_by_name_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
) -> AppResult<Option<Resolution>> {
    Ok(sqlx::query_as::<_, Resolution>(
        "SELECT id, project_id, name, position FROM resolutions \
         WHERE project_id = ? AND name = ?",
    )
    .bind(project_id)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?)
}

/// A project's default resolution: the lowest position.
///
/// What a move into a done status auto-sets when the client did not name one.
pub async fn default_resolution_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
) -> AppResult<Option<Resolution>> {
    Ok(sqlx::query_as::<_, Resolution>(
        "SELECT id, project_id, name, position FROM resolutions \
         WHERE project_id = ? ORDER BY position, name LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Inserts a resolution.
pub async fn insert_resolution(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    name: &str,
    position: i64,
) -> AppResult<Resolution> {
    let id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO resolutions (id, project_id, name, position) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(project_id)
        .bind(name)
        .bind(position)
        .execute(&mut *tx)
        .await?;

    Ok(Resolution {
        id,
        project_id: project_id.to_owned(),
        name: name.to_owned(),
        position,
    })
}

/// Edits a resolution.
pub async fn update_resolution(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    name: Option<&str>,
    position: Option<i64>,
) -> AppResult<Resolution> {
    sqlx::query(
        "UPDATE resolutions SET \
           name     = COALESCE(?, name), \
           position = COALESCE(?, position) \
         WHERE id = ?",
    )
    .bind(name)
    .bind(position)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    sqlx::query_as::<_, Resolution>(
        "SELECT id, project_id, name, position FROM resolutions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
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

    async fn project(db: &Db) -> String {
        let mut tx = db.begin_write().await.unwrap();
        let project = project::insert(
            &mut tx,
            &NewProject {
                key: "ATLAS".to_owned(),
                name: "Atlas".to_owned(),
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

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("  In Review  ").unwrap(), "In Review");
        assert!(validate_name("").is_err());
        assert!(validate_name("In\nReview").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
    }

    #[tokio::test]
    async fn levels_come_back_deepest_last() {
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        // Inserted out of order on purpose: the ordering is the query's job.
        insert_level(&mut tx, &project_id, 0, "Model")
            .await
            .unwrap();
        insert_level(&mut tx, &project_id, 2, "Collection")
            .await
            .unwrap();
        insert_level(&mut tx, &project_id, -1, "Step")
            .await
            .unwrap();
        insert_level(&mut tx, &project_id, 1, "Asset")
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let names: Vec<String> = levels(&db, &project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["Collection", "Asset", "Model", "Step"]);

        db.close().await;
    }

    #[tokio::test]
    async fn a_project_cannot_have_two_levels_with_the_same_number() {
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        insert_level(&mut tx, &project_id, 1, "Epic").await.unwrap();
        let clash = insert_level(&mut tx, &project_id, 1, "Initiative").await;
        assert!(clash.is_err(), "UNIQUE (project_id, level)");
        tx.rollback().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn a_card_type_cannot_claim_a_level_the_project_has_not_defined() {
        // The composite foreign key from migration 0003, and the storage-level
        // half of ADR 0002's `parent.level > child.level` rule: a type on a rung
        // that does not exist would make the rule unanswerable.
        //
        // The FK is DEFERRABLE INITIALLY DEFERRED, so the violation surfaces at
        // COMMIT rather than at the INSERT.
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        insert_level(&mut tx, &project_id, 0, "Card").await.unwrap();
        insert_card_type(&mut tx, &project_id, "Ghost", None, None, 7, false)
            .await
            .unwrap();
        let committed = tx.commit().await;
        assert!(
            committed.is_err(),
            "a card type at an undefined level must not commit"
        );

        db.close().await;
    }

    #[tokio::test]
    async fn the_default_card_type_prefers_the_flagged_one() {
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        insert_level(&mut tx, &project_id, 1, "Epic").await.unwrap();
        insert_level(&mut tx, &project_id, 0, "Story")
            .await
            .unwrap();
        insert_card_type(&mut tx, &project_id, "Epic", None, None, 1, false)
            .await
            .unwrap();
        insert_card_type(&mut tx, &project_id, "Bug", None, None, 0, false)
            .await
            .unwrap();
        let story = insert_card_type(&mut tx, &project_id, "Story", None, None, 0, true)
            .await
            .unwrap();

        let default = default_card_type_tx(&mut tx, &project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(default.id, story.id);

        // ...and at a level, the flagged one wins there too.
        let at_zero = card_type_at_level_tx(&mut tx, &project_id, 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(at_zero.id, story.id);
        assert!(
            card_type_at_level_tx(&mut tx, &project_id, 9)
                .await
                .unwrap()
                .is_none()
        );
        tx.commit().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn config_lookups_are_scoped_to_their_project() {
        // The scoping is the guard. `cards.status_id` references `statuses(id)`,
        // which says "a status" — not "a status of this card's project". Without
        // the project in the WHERE clause, one project's board could be handed
        // another project's column and the FK would be perfectly happy.
        let (db, _temp) = db().await;
        let mine = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        let theirs = project::insert(
            &mut tx,
            &NewProject {
                key: "OTHER".to_owned(),
                name: "Other".to_owned(),
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

        let foreign_status = insert_status(&mut tx, &theirs.id, "To Do", StatusCategory::Todo, 1)
            .await
            .unwrap();

        assert!(
            find_status_tx(&mut tx, &mine, &foreign_status.id)
                .await
                .unwrap()
                .is_none(),
            "another project's status must not resolve here"
        );
        assert!(
            find_status_tx(&mut tx, &theirs.id, &foreign_status.id)
                .await
                .unwrap()
                .is_some()
        );
        tx.commit().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn statuses_map_across_projects_by_category() {
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        insert_status(&mut tx, &project_id, "Interested", StatusCategory::Todo, 1)
            .await
            .unwrap();
        let applied = insert_status(
            &mut tx,
            &project_id,
            "Applied",
            StatusCategory::InProgress,
            2,
        )
        .await
        .unwrap();
        insert_status(&mut tx, &project_id, "Offer", StatusCategory::InProgress, 6)
            .await
            .unwrap();
        let accepted = insert_status(&mut tx, &project_id, "Accepted", StatusCategory::Done, 7)
            .await
            .unwrap();

        // The first *by position* in the category, not just any of them.
        let in_progress =
            first_status_in_category_tx(&mut tx, &project_id, StatusCategory::InProgress)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(in_progress.id, applied.id);

        let done = first_status_in_category_tx(&mut tx, &project_id, StatusCategory::Done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.id, accepted.id);

        // A new card lands in the first column overall.
        let first = first_status_tx(&mut tx, &project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.name, "Interested");
        tx.commit().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn the_database_rejects_a_fourth_status_category() {
        // Three categories, enforced by the CHECK independently of
        // StatusCategory's Decode impl.
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let err = sqlx::query(
            "INSERT INTO statuses (id, project_id, name, category, position) \
             VALUES ('x', ?, 'Blocked', 'blocked', 1)",
        )
        .bind(&project_id)
        .execute(db.writer())
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");

        db.close().await;
    }

    #[tokio::test]
    async fn priorities_come_back_most_urgent_first_and_the_default_resolution_is_position_one() {
        let (db, _temp) = db().await;
        let project_id = project(&db).await;

        let mut tx = db.begin_write().await.unwrap();
        insert_priority(&mut tx, &project_id, "Low", None, None, 4)
            .await
            .unwrap();
        insert_priority(&mut tx, &project_id, "Highest", None, None, 1)
            .await
            .unwrap();
        insert_priority(&mut tx, &project_id, "Medium", None, None, 3)
            .await
            .unwrap();

        insert_resolution(&mut tx, &project_id, "Won't Do", 2)
            .await
            .unwrap();
        let done = insert_resolution(&mut tx, &project_id, "Done", 1)
            .await
            .unwrap();

        let default = default_resolution_tx(&mut tx, &project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(default.id, done.id);
        tx.commit().await.unwrap();

        // Lower rank = more urgent, so ORDER BY rank is "most urgent first".
        // This is what makes `priority > High` answerable in Phase 6.
        let names: Vec<String> = priorities(&db, &project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["Highest", "Medium", "Low"]);

        db.close().await;
    }
}
