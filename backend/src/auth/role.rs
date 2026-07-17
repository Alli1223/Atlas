//! The three Atlas roles.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, Sqlite, Type};
use utoipa::ToSchema;

/// Why a role could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown role {0:?}: expected one of admin, member, viewer")]
pub struct RoleError(String);

/// What a user is allowed to do, instance-wide.
///
/// Three roles replace Jira's 40-permission × 8-grantee-type × N-scheme matrix —
/// the single largest source of "why can't I change this" in Jira, and pure cost
/// at Atlas's scale. Per-project access (Owner / Member / Viewer) is a separate,
/// later concern; this is the instance-level role.
///
/// # Ordering
///
/// The variants are declared least- to most-privileged, so the derived `Ord`
/// *is* the privilege ordering: `Viewer < Member < Admin`. That is what makes
/// [`Role::at_least`] a comparison rather than a match arm that someone has to
/// remember to extend. Reordering the variants silently changes authorisation,
/// which is why a test pins it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read-only.
    Viewer,
    /// Can create and edit cards.
    Member,
    /// Can do everything, including managing users.
    Admin,
}

impl Role {
    /// The role's database and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Member => "member",
            Self::Admin => "admin",
        }
    }

    /// Whether this role is at least as privileged as `required`.
    pub fn at_least(self, required: Self) -> bool {
        self >= required
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = RoleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "viewer" => Ok(Self::Viewer),
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            other => Err(RoleError(other.to_owned())),
        }
    }
}

// --- sqlx integration: store as TEXT, validate on read ----------------------
//
// The same shape as `Rank`'s: the database CHECK constraint and this Decode impl
// are two independent guards against a role that means nothing.

impl Type<Sqlite> for Role {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Role {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Role {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_ordering_is_admin_over_member_over_viewer() {
        // Authorisation is `>=` on this ordering. If someone reorders the enum
        // variants, every role guard silently inverts — so pin it here.
        assert!(Role::Admin > Role::Member);
        assert!(Role::Member > Role::Viewer);
        assert!(Role::Admin > Role::Viewer);
    }

    #[test]
    fn at_least_accepts_equal_and_higher_only() {
        assert!(Role::Admin.at_least(Role::Admin));
        assert!(Role::Admin.at_least(Role::Member));
        assert!(Role::Admin.at_least(Role::Viewer));

        assert!(Role::Member.at_least(Role::Member));
        assert!(Role::Member.at_least(Role::Viewer));
        assert!(!Role::Member.at_least(Role::Admin));

        assert!(Role::Viewer.at_least(Role::Viewer));
        assert!(!Role::Viewer.at_least(Role::Member));
        assert!(!Role::Viewer.at_least(Role::Admin));
    }

    #[test]
    fn round_trips_through_its_database_spelling() {
        for role in [Role::Viewer, Role::Member, Role::Admin] {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
    }

    #[test]
    fn an_unknown_role_is_rejected_rather_than_defaulted() {
        // Defaulting an unrecognised role to Viewer would be "safe"; defaulting
        // it to anything at all hides a corrupt row. Fail instead.
        assert!("superuser".parse::<Role>().is_err());
        assert!(
            "Admin".parse::<Role>().is_err(),
            "the spelling is lowercase"
        );
        assert!(String::new().parse::<Role>().is_err());
    }

    #[test]
    fn json_uses_the_lowercase_spelling() {
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), "\"admin\"");
        assert_eq!(
            serde_json::from_str::<Role>("\"viewer\"").unwrap(),
            Role::Viewer
        );
    }
}
