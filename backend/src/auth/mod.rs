//! Authentication and authorisation.
//!
//! ## Shape
//!
//! - [`password`] — Argon2id hashing and the password policy.
//! - [`user`] — the `users` row, its API DTO, and the queries over it.
//! - [`session`] — server-side sessions and the cookie that carries them.
//! - [`lockout`] — per-username and per-IP failure counters.
//! - [`events`] — the auth audit log.
//! - [`extract`] — [`extract::CurrentUser`] and the role guards.
//! - [`middleware`] — the origin check, session loading, and the forced-reset gate.
//! - [`project_access`] — the per-project gate: which project a route is about,
//!   and the least role that may call it. The rules and rows it consults live in
//!   [`crate::domain::member`].
//! - [`seed`] — the default `Admin` account.
//! - [`problem`] — problem documents for the two conditions [`crate::error::AppError`]'s
//!   taxonomy cannot express.
//!
//! The HTTP handlers live in [`crate::api::auth`] and [`crate::api::users`]:
//! this module is the domain, `api` is the surface.
//!
//! ## Why sessions and not JWTs
//!
//! Every operation Atlas needs — force logout, deactivate a user, rotate on
//! password change — is instant invalidation, and a JWT is valid until it
//! expires. Bolting a denylist onto a JWT reintroduces the session lookup while
//! keeping the complexity. A token the SPA can read is also exfiltratable by any
//! XSS, whereas an `HttpOnly` cookie is not. See `docs/research/rust-stack.md` §4.
//!
//! ## Database access
//!
//! Every query here uses the **runtime** `sqlx::query_as::<_, T>("...")` API
//! rather than the `query_as!` macro. The macro would give compile-time schema
//! verification, but it demands either a live database at build time or a
//! committed `.sqlx/` directory that goes stale silently and breaks CI whenever
//! a query changes. Phase 1 made the same call — `db/mod.rs` and `db/migrate.rs`
//! are runtime-API throughout — and a module that switched conventions would
//! impose the offline-metadata burden on the whole workspace. Every SQL string
//! here is a `&'static str`, which satisfies sqlx 0.9's `SqlSafeStr` bound
//! without `AssertSqlSafe`; no query is ever built by formatting.

pub mod events;
pub mod extract;
pub mod lockout;
pub mod middleware;
pub mod password;
pub mod problem;
pub mod project_access;
pub mod role;
pub mod seed;
pub mod session;
pub mod user;

pub use extract::{CurrentUser, RequireAdmin};
pub use role::Role;
pub use session::Session;
pub use user::{User, UserDto};

use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};

/// The current instant, at the resolution Atlas actually stores.
///
/// **Use this rather than `Utc::now()` anywhere the value will be written to the
/// database or compared against something that was.**
///
/// [`to_sql_timestamp`] renders microseconds, so `Utc::now()`'s nanoseconds do
/// not survive a round-trip. That is harmless right up until something compares
/// an in-memory instant with the one that came back out — a session's
/// `last_seen_at`, a lock's `locked_until` — and finds them 400 nanoseconds
/// apart for no reason a reader could ever guess. Truncating at the source means
/// the application clock and the storage format have the same resolution, so an
/// instant that goes in comes back out equal to itself.
pub(crate) fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6)
}

/// Renders a timestamp the way every Atlas `TEXT` timestamp column stores it.
///
/// Fixed offset (`+00:00`) and RFC 3339, so the text form of a later instant
/// always sorts after an earlier one under SQLite's `BINARY` collation. That
/// property is what lets `WHERE expires_at < ?` work on a `TEXT` column.
///
/// Microsecond precision is chosen rather than `AutoSi` because `AutoSi` varies
/// the fraction's width with the value, and Atlas compares these strings.
///
/// **Bind timestamps through this function, never a bare `DateTime<Utc>`.**
/// sqlx's own `Encode` for `DateTime<Tz>` uses `SecondsFormat::AutoSi`, so a
/// parameter bound directly would be rendered in a *different* width from the
/// stored text and the comparison would be against the wrong string. Reading is
/// safe either way: sqlx's `Decode` parses RFC 3339 before anything else.
pub(crate) fn to_sql_timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamp_text_sorts_chronologically() {
        // The whole reason for pinning the format: SQLite compares these as
        // text. A variable-width fraction would sort 12:00:00.5 before
        // 12:00:00.123456, which is backwards.
        let earlier = Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, 0).unwrap();
        let mid = earlier + chrono::Duration::microseconds(123_456);
        let later = earlier + chrono::Duration::milliseconds(500);
        let much_later = earlier + chrono::Duration::seconds(1);

        let rendered: Vec<String> = [earlier, mid, later, much_later]
            .into_iter()
            .map(to_sql_timestamp)
            .collect();

        let mut sorted = rendered.clone();
        sorted.sort();
        assert_eq!(
            sorted, rendered,
            "text order must match chronological order"
        );
    }

    #[test]
    fn timestamps_are_fixed_width_utc() {
        let at = Utc.with_ymd_and_hms(2026, 7, 16, 9, 41, 7).unwrap();
        assert_eq!(to_sql_timestamp(at), "2026-07-16T09:41:07.000000+00:00");
    }

    #[test]
    fn the_clock_has_the_same_resolution_as_the_storage_format() {
        // The property the rest of this module relies on: an instant from
        // `now()` survives a render-and-parse round-trip *equal to itself*.
        // `Utc::now()` does not, because its nanoseconds are truncated on the
        // way to text.
        let truncated = now();
        let rendered = to_sql_timestamp(truncated);
        let parsed = DateTime::parse_from_rfc3339(&rendered)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(parsed, truncated);

        // And the failure it prevents, made explicit.
        let raw = Utc.with_ymd_and_hms(2026, 7, 16, 9, 41, 7).unwrap()
            + chrono::Duration::nanoseconds(123_456_789);
        let round_tripped = DateTime::parse_from_rfc3339(&to_sql_timestamp(raw))
            .unwrap()
            .with_timezone(&Utc);
        assert_ne!(round_tripped, raw, "nanoseconds are not stored");
        assert_eq!(round_tripped, raw.trunc_subsecs(6));
    }
}
