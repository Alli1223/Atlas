//! The domain model: projects, the configurable hierarchy, cards, and history.
//!
//! ## Shape
//!
//! - [`project`] — the `projects` row, and the atomic card-key counter.
//! - [`member`] — `project_members`, and the rules deciding what one person may
//!   do on one project. Enforced by [`crate::auth::project_access`].
//! - [`config`] — hierarchy levels, card types, statuses, priorities, resolutions.
//!   Flat and per-project; there are no schemes (docs/adr/0003).
//! - [`hierarchy`] — ancestor/descendant walks, the depth cap, cycle detection.
//! - [`card`] — the `cards` row and every mutation, including the diffing
//!   [`card::update`] that writes history so a handler cannot forget to.
//! - [`history`] — the `card_history` changelog.
//! - [`comment`] — the `comments` row.
//! - [`tag`] — free-text labels, and the presets each template seeds.
//! - [`template`] — the four project templates' seed data.
//!
//! The HTTP handlers live in [`crate::api`]: this module is the domain, `api` is
//! the surface. That is the same split [`crate::auth`] makes.
//!
//! ## The one idea to hold on to
//!
//! **Hierarchy is per-project configuration over a uniform `parent_id`.** There
//! is no `Epic` in this code, no `is_subtask` flag, and no level named anywhere
//! outside [`template`]'s seed data. A card that contains a board is just a card
//! with children. Read `docs/adr/0002` before changing anything here.
//!
//! ## Database access
//!
//! The **runtime** `sqlx::query_as::<_, T>("...")` API throughout, never the
//! `query_as!` macro — the same call [`crate::auth`] and [`crate::db`] made, for
//! the same reason: the macro needs a live database at build time or a committed
//! `.sqlx/` directory that goes stale silently and breaks CI whenever a query
//! changes. Every SQL string here is a `&'static str`, which satisfies sqlx
//! 0.9's `SqlSafeStr` bound without `AssertSqlSafe` — so the absence of
//! `AssertSqlSafe` in this module is a real signal that no SQL is built by
//! formatting.

pub mod card;
pub mod comment;
pub mod config;
pub mod hierarchy;
pub mod history;
pub mod member;
pub mod project;
pub mod tag;
pub mod template;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, Sqlite, Type};
use utoipa::ToSchema;

pub use card::{Card, CardDto, CardPatch};
pub use project::{Project, ProjectDto};
pub use tag::{Tag, TagColour, TagUsage};

/// Why a status category could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown status category {0:?}: expected one of todo, in_progress, done")]
pub struct StatusCategoryError(String);

/// The three buckets every status falls into. Exactly three, forever.
///
/// # Why three, and why this is not a limitation worth removing
///
/// Statuses are unlimited and per-project — job search seeds nine of them. The
/// *categories* are fixed because every consumer of a status is really asking a
/// three-way question: the board's done-column styling, the cumulative flow
/// diagram's bands, burndown's "is this finished", `resolution IS EMPTY`, and
/// "can I close the parent". A fourth category means nothing to any of them.
///
/// Jira hardcodes three and refuses to add more, and that is the one piece of
/// Jira's status model worth copying exactly.
///
/// # Ordering
///
/// Declared in workflow order, so the derived `Ord` is progress order:
/// `Todo < InProgress < Done`. Reports depend on it; a test pins it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    /// Not started.
    Todo,
    /// Started, not finished.
    InProgress,
    /// Finished. Entering this category is what sets a resolution.
    Done,
}

impl StatusCategory {
    /// The category's database and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    /// Whether a card in this category counts as finished.
    ///
    /// The single place the question is answered, so "done" cannot come to mean
    /// two subtly different things in two modules.
    pub fn is_done(self) -> bool {
        self == Self::Done
    }
}

impl fmt::Display for StatusCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StatusCategory {
    type Err = StatusCategoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(Self::Todo),
            "in_progress" => Ok(Self::InProgress),
            "done" => Ok(Self::Done),
            other => Err(StatusCategoryError(other.to_owned())),
        }
    }
}

// The same sqlx shape as `Role` and `Rank`: stored as TEXT, validated on read.
// The CHECK constraint in the migration and this Decode impl are two independent
// guards against a category that means nothing.

impl Type<Sqlite> for StatusCategory {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for StatusCategory {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for StatusCategory {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// Why an estimation unit could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown estimation unit {0:?}")]
pub struct EstimationUnitError(String);

/// How a project's single `estimate` field is interpreted.
///
/// **One field.** Jira has two, both displayed as "Story Points", and which one
/// a board reads depends on configuration nobody can find — the single most
/// durable scar in its data model. Atlas stores one number per card and this
/// enum says what the number means.
///
/// [`EstimationUnit::None`] is a first-class answer, not an unconfigured state:
/// reports degrade to counting cards rather than demanding a number the user has
/// no reason to supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EstimationUnit {
    /// Story points.
    Points,
    /// Hours.
    Hours,
    /// Days.
    Days,
    /// T-shirt sizes, mapped to a number for arithmetic.
    Tshirt,
    /// A plain count of things.
    Count,
    /// No estimation. Reports fall back to card counts.
    #[default]
    None,
}

impl EstimationUnit {
    /// The unit's database and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Points => "points",
            Self::Hours => "hours",
            Self::Days => "days",
            Self::Tshirt => "tshirt",
            Self::Count => "count",
            Self::None => "none",
        }
    }
}

impl fmt::Display for EstimationUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EstimationUnit {
    type Err = EstimationUnitError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "points" => Ok(Self::Points),
            "hours" => Ok(Self::Hours),
            "days" => Ok(Self::Days),
            "tshirt" => Ok(Self::Tshirt),
            "count" => Ok(Self::Count),
            "none" => Ok(Self::None),
            other => Err(EstimationUnitError(other.to_owned())),
        }
    }
}

impl Type<Sqlite> for EstimationUnit {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for EstimationUnit {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for EstimationUnit {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_categories_order_by_progress() {
        // Reports band on this ordering. Reordering the variants silently
        // reverses every cumulative flow diagram.
        assert!(StatusCategory::Todo < StatusCategory::InProgress);
        assert!(StatusCategory::InProgress < StatusCategory::Done);
    }

    #[test]
    fn only_done_is_done() {
        assert!(StatusCategory::Done.is_done());
        assert!(!StatusCategory::InProgress.is_done());
        assert!(!StatusCategory::Todo.is_done());
    }

    #[test]
    fn status_categories_round_trip_through_their_database_spelling() {
        for category in [
            StatusCategory::Todo,
            StatusCategory::InProgress,
            StatusCategory::Done,
        ] {
            assert_eq!(
                category.as_str().parse::<StatusCategory>().unwrap(),
                category
            );
        }
    }

    #[test]
    fn a_fourth_status_category_is_rejected_rather_than_defaulted() {
        // There are three. A row saying otherwise is corrupt, and defaulting it
        // to Todo would hide that from everyone forever.
        assert!("blocked".parse::<StatusCategory>().is_err());
        assert!("Done".parse::<StatusCategory>().is_err(), "lowercase only");
        assert!(String::new().parse::<StatusCategory>().is_err());
    }

    #[test]
    fn status_category_json_matches_the_database_spelling() {
        assert_eq!(
            serde_json::to_string(&StatusCategory::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::from_str::<StatusCategory>("\"done\"").unwrap(),
            StatusCategory::Done
        );
    }

    #[test]
    fn estimation_units_round_trip_and_default_to_none() {
        // `none` is a real choice, and it is the default: a new project should
        // not demand numbers from someone who has not asked to estimate.
        assert_eq!(EstimationUnit::default(), EstimationUnit::None);

        for unit in [
            EstimationUnit::Points,
            EstimationUnit::Hours,
            EstimationUnit::Days,
            EstimationUnit::Tshirt,
            EstimationUnit::Count,
            EstimationUnit::None,
        ] {
            assert_eq!(unit.as_str().parse::<EstimationUnit>().unwrap(), unit);
        }

        assert!("story-points".parse::<EstimationUnit>().is_err());
    }
}
