//! `project_members`: who may do what, and where.
//!
//! [`crate::auth::role::Role`] says what a person may *ever* do. This module says
//! *where* they may do it. The two compose by [`ProjectRole::capped_by`], and the
//! composition is one-directional on purpose — see [`effective_role`].
//!
//! The enforcement of all this is [`crate::auth::project_access`], which is a
//! layer over the whole `/api/v1` tree. This module is only the rules and the
//! rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

use crate::auth::role::Role;
use crate::auth::to_sql_timestamp;
use crate::auth::user::User;
use crate::db::Db;
use crate::domain::project::Project;
use crate::error::{AppError, AppResult};

/// Why a project role could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown project role {0:?}: expected one of owner, member, viewer")]
pub struct ProjectRoleError(String);

/// What a user may do *on one project*.
///
/// # Ordering
///
/// Declared least- to most-privileged, so the derived `Ord` **is** the privilege
/// ordering: `Viewer < Member < Owner`. That is what makes [`Self::at_least`] a
/// comparison rather than a match arm somebody has to remember to extend, and
/// what makes [`Self::capped_by`] a `min`. Reordering these variants silently
/// inverts every authorisation decision in Atlas, which is why a test pins it.
///
/// Exactly the shape of [`crate::auth::role::Role`], deliberately: two role
/// enums that behaved differently would be two things to hold in your head.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ProjectRole {
    /// Read the project's cards, comments, tags and configuration.
    Viewer,
    /// Viewer, plus create/edit/move/delete cards, comment, and tag.
    Member,
    /// Member, plus project settings, member management, archive, and config.
    Owner,
}

impl ProjectRole {
    /// The role's database and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Member => "member",
            Self::Owner => "owner",
        }
    }

    /// Whether this role is at least as privileged as `required`.
    pub fn at_least(self, required: Self) -> bool {
        self >= required
    }

    /// This role, narrowed to what `instance` permits anywhere at all.
    ///
    /// **The instance role is a ceiling, not a floor.** An instance Viewer who
    /// holds an `owner` row resolves to [`ProjectRole::Viewer`]: "this account is
    /// read-only" is a statement about the account, and a project owner must not
    /// be able to quietly overrule it by handing out a grant. The mapping is
    /// therefore a `min` against the most a given instance role can ever hold:
    ///
    /// | instance | ceiling |
    /// |---|---|
    /// | [`Role::Viewer`] | [`ProjectRole::Viewer`] — read-only, everywhere |
    /// | [`Role::Member`] | [`ProjectRole::Owner`] — a Member who creates a project owns it |
    /// | [`Role::Admin`] | [`ProjectRole::Owner`] |
    ///
    /// Only the Viewer row actually bites today. It is written as a total
    /// function anyway so that adding a fourth instance role is a compile error
    /// here rather than a silent grant.
    #[must_use]
    pub fn capped_by(self, instance: Role) -> Self {
        let ceiling = match instance {
            Role::Viewer => Self::Viewer,
            Role::Member | Role::Admin => Self::Owner,
        };
        self.min(ceiling)
    }
}

impl fmt::Display for ProjectRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProjectRole {
    type Err = ProjectRoleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "viewer" => Ok(Self::Viewer),
            "member" => Ok(Self::Member),
            "owner" => Ok(Self::Owner),
            other => Err(ProjectRoleError(other.to_owned())),
        }
    }
}

// --- sqlx integration: store as TEXT, validate on read ----------------------
//
// The same shape as `auth::role::Role`'s: the database CHECK constraint and this
// Decode impl are two independent guards against a role that means nothing.

impl Type<Sqlite> for ProjectRole {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for ProjectRole {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for ProjectRole {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

// There is deliberately no bare `ProjectMember` row type. Nothing needs one:
// authorisation wants a single role ([`find_role`]) and the member list wants the
// person as well as the grant ([`MemberListing`]). A struct mirroring the table
// for its own sake would be a third shape to keep in step with the other two.

/// A grant joined to the person holding it, for `GET /projects/{key}/members`.
///
/// The join is here rather than two round-trips in the handler because a member
/// list with no names in it is not a member list. It carries `is_lead` as well,
/// so that [`resolve`] can be applied to the row without a further query — a
/// member list that showed the raw grant would tell an owner that the instance
/// admin they added as a viewer is a viewer, which is false.
#[derive(Debug, Clone, FromRow)]
pub struct MemberListing {
    /// Who holds the grant.
    pub user_id: String,
    /// What the row grants.
    pub role: ProjectRole,
    /// When it was granted.
    pub added_at: DateTime<Utc>,
    /// Who granted it.
    pub added_by: Option<String>,
    /// The holder's login name.
    pub username: String,
    /// The holder's display name.
    pub display_name: String,
    /// The holder's avatar.
    pub avatar_url: Option<String>,
    /// The holder's **instance** role — the ceiling on `role`.
    pub instance_role: Role,
    /// Whether the holder can sign in at all.
    pub is_active: bool,
    /// Whether this person is the project's lead, and so an implicit owner.
    pub is_lead: bool,
}

/// A project member as the API describes it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMemberDto {
    /// Who holds the grant.
    pub user_id: String,
    /// The holder's login name.
    pub username: String,
    /// The holder's display name.
    pub display_name: String,
    /// The holder's avatar.
    pub avatar_url: Option<String>,
    /// **What the row says**, before the instance-role ceiling.
    pub role: ProjectRole,
    /// **What the holder can actually do**, after the ceiling.
    ///
    /// Both are on the wire because they can differ, and a member list that
    /// showed only `role` would tell an owner that their read-only colleague is
    /// an owner. See [`ProjectRole::capped_by`].
    pub effective_role: ProjectRole,
    /// The holder's instance role, which is what does the capping.
    pub instance_role: Role,
    /// Whether the holder can sign in at all.
    pub is_active: bool,
    /// When the grant was made.
    pub added_at: DateTime<Utc>,
    /// Who made it. `null` means the 0005 backfill rather than a person.
    pub added_by: Option<String>,
}

impl From<&MemberListing> for ProjectMemberDto {
    fn from(row: &MemberListing) -> Self {
        Self {
            user_id: row.user_id.clone(),
            username: row.username.clone(),
            display_name: row.display_name.clone(),
            avatar_url: row.avatar_url.clone(),
            role: row.role,
            // The same `resolve` every authorisation decision goes through, so
            // the list cannot describe an access rule the gate does not apply.
            effective_role: resolve(row.instance_role, row.is_lead, Some(row.role))
                .unwrap_or(row.role),
            instance_role: row.instance_role,
            is_active: row.is_active,
            added_at: row.added_at,
            added_by: row.added_by.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution — the one function every authorisation decision goes through
// ---------------------------------------------------------------------------

/// The access rules themselves, with every lookup already done.
///
/// **This is the whole of Atlas's per-project authorisation policy.** It is a
/// pure function of three facts so that it can be exhaustively unit-tested and
/// so that there is exactly one copy of the rules — [`effective_role`] does the
/// queries and calls this, and [`ProjectMemberDto`] renders this rather than
/// re-deriving it. A second implementation of these rules anywhere would be a
/// second thing to keep in sync, and the failure mode of them drifting apart is
/// silent.
///
/// The rules, in order, exactly as migration 0005 documents them:
///
/// 1. **Instance admin → owner**, on every project, with no row needed. Without
///    this an admin can be locked out of a project they are nonetheless
///    responsible for administering, and no API call can repair it.
/// 2. **The project's lead → owner.** The lead is who the project points at; an
///    explicit `viewer` row cannot demote them, because the way to stop someone
///    leading a project is to change `lead_id`.
/// 3. **A `project_members` row → whatever it says.**
/// 4. **Otherwise → `None`.** Default deny. No row is no access.
///
/// Whatever comes out is then capped by [`ProjectRole::capped_by`], so a grant
/// can narrow an instance role but never widen it.
#[must_use]
pub fn resolve(instance: Role, is_lead: bool, granted: Option<ProjectRole>) -> Option<ProjectRole> {
    let base = if instance == Role::Admin || is_lead {
        Some(ProjectRole::Owner)
    } else {
        granted
    };

    base.map(|role| role.capped_by(instance))
}

/// What `user` may do on `project`, or `None` for no access at all.
///
/// The lookups; [`resolve`] is the rules.
pub async fn effective_role(
    db: &Db,
    project: &Project,
    user: &User,
) -> AppResult<Option<ProjectRole>> {
    let is_lead = project.lead_id.as_deref() == Some(user.id.as_str());

    // Admins and leads are owners by rule, so the grant lookup is skipped for
    // them: it could not change the answer, and this is on the hot path of every
    // single project-scoped request.
    let granted = if user.role == Role::Admin || is_lead {
        None
    } else {
        find_role(db, &project.id, &user.id).await?
    };

    Ok(resolve(user.role, is_lead, granted))
}

/// [`effective_role`], or the error the caller should return.
///
/// # Errors
///
/// - [`AppError::NotFound`] (404) when the user has **no access at all**. Not a
///   403: a 403 confirms the project exists, which hands an outsider the key
///   namespace one guess at a time. To someone with no grant, an inaccessible
///   project is indistinguishable from a project that was never created.
/// - [`AppError::Forbidden`] (403) when the user has access but not enough of
///   it. Here 404 would be actively misleading — they can see the project in
///   their own list, so "it does not exist" is a lie they can immediately
///   disprove. They know it is there; they are being told they may not.
pub async fn require(
    db: &Db,
    project: &Project,
    user: &User,
    min: ProjectRole,
) -> AppResult<ProjectRole> {
    let Some(role) = effective_role(db, project, user).await? else {
        return Err(AppError::NotFound);
    };

    if role.at_least(min) {
        Ok(role)
    } else {
        Err(AppError::Forbidden)
    }
}

/// The raw grant on a project, ignoring both implicit ownership and the ceiling.
///
/// Almost never what you want — [`effective_role`] is. This exists for the
/// member-management handlers, which edit the rows rather than consult them.
pub async fn find_role(db: &Db, project_id: &str, user_id: &str) -> AppResult<Option<ProjectRole>> {
    Ok(
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id = ? AND user_id = ?")
            .bind(project_id)
            .bind(user_id)
            .fetch_optional(db.reader())
            .await?,
    )
}

/// [`find_role`], inside an open transaction.
pub async fn find_role_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    user_id: &str,
) -> AppResult<Option<ProjectRole>> {
    Ok(
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id = ? AND user_id = ?")
            .bind(project_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?,
    )
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Every grant on a project, with the person holding it, by display name.
///
/// # What is not in here
///
/// Instance admins with no row. They are owners of every project by rule 1 of
/// [`resolve`], and listing every one of them on every project would turn the
/// member list into a copy of the user list. This is the **grant** list: it
/// answers "who has been given access to this project", not "who can reach it".
///
/// `COALESCE(..., 0)` on `is_lead` because `lead_id = user_id` is NULL, not 0,
/// when the project has no lead — and NULL would fail to decode into a `bool`.
pub async fn list(db: &Db, project_id: &str) -> AppResult<Vec<MemberListing>> {
    Ok(sqlx::query_as::<_, MemberListing>(
        "SELECT m.user_id, m.role, m.added_at, m.added_by, \
                u.username, u.display_name, u.avatar_url, \
                u.role AS instance_role, u.is_active, \
                COALESCE(p.lead_id = m.user_id, 0) AS is_lead \
           FROM project_members m \
           JOIN users u ON u.id = m.user_id \
           JOIN projects p ON p.id = m.project_id \
          WHERE m.project_id = ? \
          ORDER BY u.display_name, u.username",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// Grants a project role, failing if the user already has one.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    user_id: &str,
    role: ProjectRole,
    added_by: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role, added_at, added_by) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role)
    .bind(to_sql_timestamp(now))
    .bind(added_by)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Grants a project role, leaving an existing grant alone.
///
/// For project creation, where the creator and the lead are frequently the same
/// person and inserting them twice must not be an error.
pub async fn insert_or_ignore(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    user_id: &str,
    role: ProjectRole,
    added_by: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO project_members (project_id, user_id, role, added_at, added_by) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role)
    .bind(to_sql_timestamp(now))
    .bind(added_by)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Changes an existing grant. Returns whether a row moved.
pub async fn update_role(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    user_id: &str,
    role: ProjectRole,
) -> AppResult<bool> {
    let result =
        sqlx::query("UPDATE project_members SET role = ? WHERE project_id = ? AND user_id = ?")
            .bind(role)
            .bind(project_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// Revokes a grant. Returns whether a row went.
pub async fn delete(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    user_id: &str,
) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM project_members WHERE project_id = ? AND user_id = ?")
        .bind(project_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Whether an `owner` grant held by `user` actually makes them one.
///
/// A row is not an owner; a row *plus a person* is. Two things can make an
/// `owner` row grant nothing:
///
/// - the holder is an instance Viewer, so [`ProjectRole::capped_by`] narrows it
///   to `viewer`;
/// - the holder is deactivated, so they cannot make any request at all.
///
/// This exists because the last-owner guard has to count people who can do the
/// job, not rows that look like they could. It is the same distinction
/// [`crate::auth::user::active_admin_count`] draws with its `is_active = 1`: an
/// inactive admin cannot unlock anyone, so it must not satisfy the guard.
#[must_use]
pub fn grants_ownership(role: ProjectRole, user: &User) -> bool {
    user.is_active && role.capped_by(user.role) == ProjectRole::Owner
}

/// How many people a project's member list holds who can actually own it.
///
/// The last-owner guard's counter, and the analogue of
/// [`crate::auth::user::active_admin_count`].
///
/// # What this counts, and why it is not just `role = 'owner'`
///
/// An `owner` row held by an instance Viewer grants `viewer`, and one held by a
/// deactivated account grants nothing to anybody. Counting either would let the
/// guard be satisfied by a row that cannot manage the project — so demoting the
/// last *real* owner would sail through, leaving exactly the ownerless member
/// list the guard exists to prevent. The `WHERE` below is
/// [`grants_ownership`] expressed in SQL; [`ProjectRole::capped_by`] is the
/// definition both follow.
///
/// # What this deliberately does not count
///
/// Implicit owners — instance admins, and the lead — are **not** counted. That
/// is not an oversight, and the guard is not decorative because of it:
/// `PATCH /projects/{key}` can clear `lead_id`, so a member list with no owner
/// in it is one field edit away from a project with no owner at all, reachable
/// only by an instance admin. Counting the lead here would make the guard depend
/// on a column the very same API can null out a moment later.
pub async fn owner_count(tx: &mut sqlx::SqliteConnection, project_id: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_members m \
           JOIN users u ON u.id = m.user_id \
          WHERE m.project_id = ? \
            AND m.role = 'owner' \
            AND u.is_active = 1 \
            AND u.role != 'viewer'",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await?)
}

/// The keys of the projects `user` is the **only** effective owner of.
///
/// # What this is for
///
/// [`owner_count`] answers the last-owner question from the project's side, for
/// the member routes. This answers it from the *account's* side, for the user
/// routes — because deactivating someone, or making them a read-only account,
/// silently strips every `owner` row they hold of its meaning
/// ([`grants_ownership`]). Those routes never name a project, so without this
/// they cannot tell that they are about to orphan four of them.
///
/// # Why it loops rather than doing it in one query
///
/// The clause distinguishing "an owner row" from "an owner" is subtle enough
/// that a second copy of it is a liability — [`owner_count`]'s doc comment is
/// three paragraphs about exactly which rows must not be counted. Expressing this
/// as one query means restating that predicate under a second pair of table
/// aliases, and the failure mode of the two drifting apart is a guard that
/// protects the wrong set of projects, silently.
///
/// So this asks [`owner_count`] — the single definition — once per project the
/// user holds an `owner` row on. That is a handful of cheap indexed counts on a
/// path taken only when an account is being deactivated or demoted, which is not
/// a hot path and never will be.
pub async fn projects_solely_owned_by(
    tx: &mut sqlx::SqliteConnection,
    user: &User,
) -> AppResult<Vec<String>> {
    // An account that cannot own anything is not the last owner of anything, and
    // skipping here is not just an optimisation: `owner_count` would return 0 for
    // a project whose only `owner` row is theirs, and `<= 1` would then report it
    // as solely owned by somebody who does not own it at all.
    if !grants_ownership(ProjectRole::Owner, user) {
        return Ok(Vec::new());
    }

    let held: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.id, p.key FROM projects p \
           JOIN project_members m ON m.project_id = p.id \
          WHERE m.user_id = ? AND m.role = 'owner' \
          ORDER BY p.key",
    )
    .bind(&user.id)
    .fetch_all(&mut *tx)
    .await?;

    let mut solely = Vec::new();
    for (project_id, project_key) in held {
        // They hold an owner row and can own, so they are one of the owners this
        // counts: a count of one is a count of them.
        if owner_count(&mut *tx, &project_id).await? <= 1 {
            solely.push(project_key);
        }
    }

    Ok(solely)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_ordering_is_owner_over_member_over_viewer() {
        // Authorisation is `>=` on this ordering. Reordering the enum variants
        // silently inverts every project guard, so pin it.
        assert!(ProjectRole::Owner > ProjectRole::Member);
        assert!(ProjectRole::Member > ProjectRole::Viewer);
        assert!(ProjectRole::Owner > ProjectRole::Viewer);
    }

    #[test]
    fn at_least_accepts_equal_and_higher_only() {
        assert!(ProjectRole::Owner.at_least(ProjectRole::Owner));
        assert!(ProjectRole::Owner.at_least(ProjectRole::Member));
        assert!(ProjectRole::Owner.at_least(ProjectRole::Viewer));

        assert!(ProjectRole::Member.at_least(ProjectRole::Member));
        assert!(ProjectRole::Member.at_least(ProjectRole::Viewer));
        assert!(!ProjectRole::Member.at_least(ProjectRole::Owner));

        assert!(ProjectRole::Viewer.at_least(ProjectRole::Viewer));
        assert!(!ProjectRole::Viewer.at_least(ProjectRole::Member));
        assert!(!ProjectRole::Viewer.at_least(ProjectRole::Owner));
    }

    #[test]
    fn the_instance_role_is_a_ceiling_and_never_a_floor() {
        // The rule the whole module turns on. An instance Viewer cannot be a
        // project owner no matter what row somebody writes.
        assert_eq!(
            ProjectRole::Owner.capped_by(Role::Viewer),
            ProjectRole::Viewer
        );
        assert_eq!(
            ProjectRole::Member.capped_by(Role::Viewer),
            ProjectRole::Viewer
        );
        assert_eq!(
            ProjectRole::Viewer.capped_by(Role::Viewer),
            ProjectRole::Viewer
        );

        // An instance Member may own a project — they own the ones they create.
        assert_eq!(
            ProjectRole::Owner.capped_by(Role::Member),
            ProjectRole::Owner
        );
        assert_eq!(
            ProjectRole::Owner.capped_by(Role::Admin),
            ProjectRole::Owner
        );

        // ...and it is a ceiling, not a floor: being an instance Admin does not
        // promote a `viewer` grant. (Admins get Owner from `effective_role`'s
        // first rule instead, which is a different mechanism.)
        assert_eq!(
            ProjectRole::Viewer.capped_by(Role::Admin),
            ProjectRole::Viewer
        );
        assert_eq!(
            ProjectRole::Member.capped_by(Role::Admin),
            ProjectRole::Member
        );
    }

    #[test]
    fn no_grant_is_no_access() {
        // Rule 4, the one the whole phase turns on. An ordinary user with no row
        // gets None — not Viewer, not "read-only by default". Default deny.
        assert_eq!(resolve(Role::Member, false, None), None);
        assert_eq!(resolve(Role::Viewer, false, None), None);
    }

    #[test]
    fn an_instance_admin_owns_every_project_without_a_row() {
        // Rule 1. Otherwise an admin can be locked out of a project they are
        // responsible for, with no way back through the API.
        assert_eq!(resolve(Role::Admin, false, None), Some(ProjectRole::Owner));
        // ...and an explicit lesser grant does not demote them: rule 1 fires
        // first. The row is not a downgrade, it is just a row.
        assert_eq!(
            resolve(Role::Admin, false, Some(ProjectRole::Viewer)),
            Some(ProjectRole::Owner)
        );
    }

    #[test]
    fn the_lead_owns_the_project_without_a_row() {
        // Rule 2, and the same non-demotion property: to stop someone leading a
        // project you change lead_id, not their grant.
        assert_eq!(resolve(Role::Member, true, None), Some(ProjectRole::Owner));
        assert_eq!(
            resolve(Role::Member, true, Some(ProjectRole::Viewer)),
            Some(ProjectRole::Owner)
        );
    }

    #[test]
    fn an_explicit_grant_is_taken_at_face_value() {
        // Rule 3.
        for role in [ProjectRole::Viewer, ProjectRole::Member, ProjectRole::Owner] {
            assert_eq!(resolve(Role::Member, false, Some(role)), Some(role));
        }
    }

    #[test]
    fn an_instance_viewer_who_is_a_project_owner_still_cannot_write() {
        // The ceiling, applied through the real entry point rather than to
        // `capped_by` in isolation — this is the exact scenario the phase brief
        // calls out, and it must hold down all three paths into a grant.
        assert_eq!(
            resolve(Role::Viewer, false, Some(ProjectRole::Owner)),
            Some(ProjectRole::Viewer),
            "an explicit owner grant"
        );
        assert_eq!(
            resolve(Role::Viewer, true, None),
            Some(ProjectRole::Viewer),
            "being the project lead"
        );
        assert_eq!(
            resolve(Role::Viewer, true, Some(ProjectRole::Owner)),
            Some(ProjectRole::Viewer),
            "both at once"
        );

        // And the ceiling narrows without granting: an instance Viewer with no
        // row still has no access at all.
        assert_eq!(resolve(Role::Viewer, false, None), None);
    }

    #[test]
    fn round_trips_through_its_database_spelling() {
        for role in [ProjectRole::Viewer, ProjectRole::Member, ProjectRole::Owner] {
            assert_eq!(role.as_str().parse::<ProjectRole>().unwrap(), role);
        }
    }

    #[test]
    fn an_unknown_project_role_is_rejected_rather_than_defaulted() {
        // Defaulting an unrecognised role to Viewer would be "safe"; defaulting
        // it to anything at all hides a corrupt row. Fail instead.
        assert!("admin".parse::<ProjectRole>().is_err());
        assert!("superuser".parse::<ProjectRole>().is_err());
        assert!(
            "Owner".parse::<ProjectRole>().is_err(),
            "spelling is lowercase"
        );
        assert!(String::new().parse::<ProjectRole>().is_err());
    }

    #[test]
    fn json_uses_the_lowercase_spelling() {
        assert_eq!(
            serde_json::to_string(&ProjectRole::Owner).unwrap(),
            "\"owner\""
        );
        assert_eq!(
            serde_json::from_str::<ProjectRole>("\"viewer\"").unwrap(),
            ProjectRole::Viewer
        );
    }
}
