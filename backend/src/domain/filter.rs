//! `filters` — saved AQL queries.
//!
//! The domain rows and their CRUD. The *meaning* of a filter — parsing its AQL,
//! inlining `filter = "…"` references, guarding cycles — lives in
//! [`crate::aql`], because that is where the query language lives. This module
//! is the storage, exactly as [`crate::domain::config`] is storage for the
//! per-project config the API layer drives.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::error::{AppError, AppResult};

/// Longest accepted filter name, in characters.
pub const MAX_NAME: usize = 128;

/// Longest accepted filter description, in characters.
pub const MAX_DESCRIPTION: usize = 4 * 1024;

/// Longest accepted AQL body — a second guard beyond the lexer's byte limit, so
/// a filter cannot be saved that would be refused the moment it is run.
pub const MAX_AQL: usize = 8 * 1024;

/// A row of `filters`, as stored and as the API describes it.
///
/// Serialisable directly: nothing here is secret, and a filter is the caller's
/// own working state.
#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    /// UUID v7, as text.
    pub id: String,
    /// Who owns it.
    pub owner_id: String,
    /// The display name, unique per owner.
    pub name: String,
    /// An optional description.
    pub description: Option<String>,
    /// The AQL source.
    pub aql: String,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// The columns of `filters`, spliced by `concat!` so every query is a
/// `&'static str`.
macro_rules! filter_columns {
    () => {
        "id, owner_id, name, description, aql, created_at, updated_at"
    };
}

/// Checks a filter name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Validation("A filter needs a name.".to_owned()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "A filter name must be at most {MAX_NAME} characters long."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "A filter name must not contain control characters.".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

/// Checks a filter description.
pub fn validate_description(description: &str) -> AppResult<String> {
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(AppError::Validation(format!(
            "A filter description must be at most {MAX_DESCRIPTION} characters long."
        )));
    }
    Ok(description.to_owned())
}

/// Checks an AQL body's length. Its *syntax* is checked by the compiler when the
/// filter is saved; this only bounds the size.
pub fn validate_aql_length(aql: &str) -> AppResult<()> {
    if aql.len() > MAX_AQL {
        return Err(AppError::Validation(format!(
            "The query is {} bytes; the limit is {MAX_AQL}.",
            aql.len()
        )));
    }
    Ok(())
}

/// A filter by id.
pub async fn find_by_id(db: &Db, id: &str) -> AppResult<Option<Filter>> {
    Ok(sqlx::query_as::<_, Filter>(concat!(
        "SELECT ",
        filter_columns!(),
        " FROM filters WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db.reader())
    .await?)
}

/// A caller's filter by name, case-insensitively.
///
/// Scoped to `owner_id`, which is what makes `filter = "My Filter"` resolve to
/// the caller's own filter and not somebody else's of the same name.
pub async fn find_by_name(db: &Db, owner_id: &str, name: &str) -> AppResult<Option<Filter>> {
    Ok(sqlx::query_as::<_, Filter>(concat!(
        "SELECT ",
        filter_columns!(),
        " FROM filters WHERE owner_id = ? AND name = ?"
    ))
    .bind(owner_id)
    .bind(name)
    .fetch_optional(db.reader())
    .await?)
}

/// Every filter a caller owns, by name.
pub async fn list_for_owner(db: &Db, owner_id: &str) -> AppResult<Vec<Filter>> {
    Ok(sqlx::query_as::<_, Filter>(concat!(
        "SELECT ",
        filter_columns!(),
        " FROM filters WHERE owner_id = ? ORDER BY name"
    ))
    .bind(owner_id)
    .fetch_all(db.reader())
    .await?)
}

/// Whether a name is taken by this owner, optionally excluding one filter id.
pub async fn name_taken(
    tx: &mut sqlx::SqliteConnection,
    owner_id: &str,
    name: &str,
    excluding_id: Option<&str>,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM filters WHERE owner_id = ? AND name = ? \
         AND (? IS NULL OR id != ?))",
    )
    .bind(owner_id)
    .bind(name)
    .bind(excluding_id)
    .bind(excluding_id)
    .fetch_one(&mut *tx)
    .await?)
}

/// Inserts a filter.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    owner_id: &str,
    name: &str,
    description: Option<&str>,
    aql: &str,
    now: DateTime<Utc>,
) -> AppResult<Filter> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "INSERT INTO filters (id, owner_id, name, description, aql, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(owner_id)
    .bind(name)
    .bind(description)
    .bind(aql)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the filter just inserted is missing")))
}

/// A filter by id inside a transaction.
pub async fn find_by_id_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Filter>> {
    Ok(sqlx::query_as::<_, Filter>(concat!(
        "SELECT ",
        filter_columns!(),
        " FROM filters WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// The fields a `PATCH /filters/{id}` may change.
#[allow(clippy::option_option)]
#[derive(Debug, Default)]
pub struct FilterPatch {
    /// The name, if changing.
    pub name: Option<String>,
    /// `None` leaves it; `Some(None)` clears it; `Some(Some(v))` sets it.
    pub description: Option<Option<String>>,
    /// The AQL, if changing.
    pub aql: Option<String>,
}

impl FilterPatch {
    /// Whether the patch changes anything.
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.description.is_none() && self.aql.is_none()
    }
}

/// Applies a patch. One fixed statement, `COALESCE`/`CASE` per column — the same
/// pattern as [`crate::domain::project::apply_patch`], and for the same reason:
/// no `SET` list assembled from a runtime shape.
pub async fn apply_patch(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    patch: &FilterPatch,
    now: DateTime<Utc>,
) -> AppResult<Filter> {
    sqlx::query(
        "UPDATE filters SET \
           name        = COALESCE(?, name), \
           description = CASE WHEN ? THEN ? ELSE description END, \
           aql         = COALESCE(?, aql), \
           updated_at  = ? \
         WHERE id = ?",
    )
    .bind(patch.name.clone())
    .bind(patch.description.is_some())
    .bind(patch.description.clone().flatten())
    .bind(patch.aql.clone())
    .bind(to_sql_timestamp(now))
    .bind(id)
    .execute(&mut *tx)
    .await?;

    find_by_id_tx(&mut *tx, id).await?.ok_or(AppError::NotFound)
}

/// Deletes a filter. Returns whether a row went.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM filters WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::role::Role;
    use crate::auth::user::{self, NewUser};
    use crate::db::migrate;
    use crate::test_support::TempDb;

    async fn setup() -> (Db, TempDb, String) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let owner = user::insert(
            &mut tx,
            &NewUser {
                username: "owner".to_owned(),
                email: None,
                display_name: "Owner".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (db, temp, owner.id)
    }

    #[test]
    fn names_are_validated() {
        assert_eq!(validate_name("  My Filter ").unwrap(), "My Filter");
        assert!(validate_name("").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_name(&"x".repeat(MAX_NAME + 1)).is_err());
    }

    #[tokio::test]
    async fn a_filter_round_trips_and_names_are_unique_per_owner() {
        let (db, _temp, owner) = setup().await;

        let mut tx = db.begin_write().await.unwrap();
        let created = insert(
            &mut tx,
            &owner,
            "My Filter",
            Some("mine"),
            "status = Done",
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let found = find_by_name(&db, &owner, "my filter")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.aql, "status = Done");

        // A second filter with the same name for the same owner is refused.
        let mut tx = db.begin_write().await.unwrap();
        assert!(
            name_taken(&mut tx, &owner, "MY FILTER", None)
                .await
                .unwrap()
        );
        let clash = insert(&mut tx, &owner, "My Filter", None, "x", crate::auth::now()).await;
        assert!(clash.is_err(), "UNIQUE (owner_id, name)");
        tx.rollback().await.unwrap();

        db.close().await;
    }

    #[tokio::test]
    async fn a_patch_updates_and_delete_removes() {
        let (db, _temp, owner) = setup().await;

        let mut tx = db.begin_write().await.unwrap();
        let created = insert(&mut tx, &owner, "F", None, "status = A", crate::auth::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let updated = apply_patch(
            &mut tx,
            &created.id,
            &FilterPatch {
                aql: Some("status = B".to_owned()),
                ..FilterPatch::default()
            },
            crate::auth::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(updated.aql, "status = B");

        let mut tx = db.begin_write().await.unwrap();
        assert!(delete(&mut tx, &created.id).await.unwrap());
        assert!(!delete(&mut tx, &created.id).await.unwrap());
        tx.commit().await.unwrap();

        assert!(find_by_id(&db, &created.id).await.unwrap().is_none());
        db.close().await;
    }
}
