//! Problem documents for the two auth conditions [`AppError`]'s taxonomy cannot
//! express.
//!
//! Everything else in this module tree returns an [`AppError`] and is rendered
//! by its `IntoResponse`. These two are different because the *client* has to
//! branch on them:
//!
//! - a 403 that means "go to the change-password screen" is not the same thing
//!   as a 403 that means "you are not allowed to do that", and a SPA that
//!   distinguishes them by matching on `detail` text is one copy-edit away from
//!   breaking;
//! - a lockout is a 429, and [`AppError`] has no 429.
//!
//! Both are still RFC 7807 documents with `urn:atlas:error:*` types, so they are
//! the same shape as every other error Atlas returns — this is an extension of
//! the taxonomy, not a parallel one. `instance` is left `None` and filled in by
//! [`crate::api::middleware::problem_instance`], exactly as `AppError`'s is.

use chrono::Duration;

use crate::error::{AppError, Problem};

/// The marker a client keys on to send the user to the change-password screen.
///
/// Machine-readable and stable: the frontend must never have to match on prose.
pub const PASSWORD_CHANGE_REQUIRED_TYPE: &str = "urn:atlas:error:password-change-required";

/// The marker for a locked-out login.
pub const LOCKED_OUT_TYPE: &str = "urn:atlas:error:locked-out";

/// 403: the account must change its password before doing anything else.
pub fn password_change_required() -> Problem {
    Problem {
        problem_type: PASSWORD_CHANGE_REQUIRED_TYPE.to_owned(),
        title: "Password Change Required".to_owned(),
        status: 403,
        detail: "This account must change its password before it can do anything else. \
                 Send the new password to POST /api/v1/auth/change-password."
            .to_owned(),
        instance: None,
    }
}

/// 429: too many failed logins for this username or from this address.
///
/// The remaining time goes in `detail` rather than a `Retry-After` header
/// because `problem_instance` rebuilds the response from this document to fill
/// in `instance`, and any header set alongside it would be dropped on the way
/// out. A field in the body is what survives.
///
/// The figure is rounded **up**. `num_minutes` truncates, so a lock with 14m59s
/// left would advertise "14 minutes" and send the user back to be refused a
/// second time — the one outcome a "try again in N" message exists to prevent.
/// Rounding up can only ever overshoot by under a minute.
pub fn locked_out(retry_after: Duration) -> Problem {
    // Clamped to at least one second before the cast: "try again in 0 minutes"
    // is worse than useless, and a negative duration would mean the caller asked
    // about a lock that has already lifted. `div_ceil` is only stable on the
    // unsigned integers, which the clamp makes safe to cast to.
    let seconds = retry_after.num_seconds().max(1).unsigned_abs();
    let minutes = seconds.div_ceil(60).max(1);
    Problem {
        problem_type: LOCKED_OUT_TYPE.to_owned(),
        title: "Too Many Attempts".to_owned(),
        status: 429,
        detail: format!(
            "Too many failed sign-in attempts. Try again in {minutes} minute{}.",
            if minutes == 1 { "" } else { "s" }
        ),
        instance: None,
    }
}

/// 401: the username or the password is wrong, and we are not saying which.
///
/// Reuses [`AppError::Unauthorized`]'s `type` and `title` so the client sees the
/// ordinary "not authenticated" shape, and replaces only the `detail`, whose
/// default ("Authentication is required to access this resource") describes a
/// missing cookie rather than a rejected password.
///
/// **This exact document is returned for a wrong password, an unknown username,
/// and a deactivated account.** Three distinguishable responses would be three
/// oracles: which usernames exist, and which accounts are disabled.
pub fn invalid_credentials() -> Problem {
    Problem {
        detail: "Invalid username or password.".to_owned(),
        ..AppError::Unauthorized.problem()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;

    async fn body(problem: Problem) -> (StatusCode, serde_json::Value) {
        let response = problem.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn the_forced_reset_marker_is_machine_readable() {
        let (status, json) = body(password_change_required()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // The whole contract with the frontend is this string.
        assert_eq!(json["type"], "urn:atlas:error:password-change-required");
        assert_eq!(json["status"], 403);
        assert!(json["detail"].is_string());
    }

    #[tokio::test]
    async fn the_forced_reset_marker_is_not_a_plain_forbidden() {
        // If these two ever collide, the SPA cannot tell "reset your password"
        // from "you are not an admin", and will redirect on both.
        let (_, gate) = body(password_change_required()).await;
        let (_, forbidden) = body(AppError::Forbidden.problem()).await;
        assert_ne!(gate["type"], forbidden["type"]);
    }

    #[tokio::test]
    async fn lockout_is_a_429_that_says_how_long() {
        let (status, json) = body(locked_out(Duration::minutes(15))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["type"], "urn:atlas:error:locked-out");
        assert!(
            json["detail"].as_str().unwrap().contains("15 minutes"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn a_sub_minute_lock_still_reports_a_whole_minute() {
        // "Try again in 0 minutes" is worse than useless.
        let (_, json) = body(locked_out(Duration::seconds(20))).await;
        assert!(
            json["detail"].as_str().unwrap().contains("1 minute."),
            "{json}"
        );
    }

    #[tokio::test]
    async fn the_remaining_time_rounds_up_so_the_user_is_never_sent_back_early() {
        // The realistic case: a lock is set for 15 minutes and read back a
        // fraction of a second later, so 14m59s remain. Truncating would say
        // "14 minutes" and the user would be refused again when they came back.
        let (_, json) = body(locked_out(Duration::seconds(15 * 60 - 1))).await;
        assert!(
            json["detail"].as_str().unwrap().contains("15 minutes"),
            "{json}"
        );

        // Exactly on a boundary, no overshoot.
        let (_, json) = body(locked_out(Duration::seconds(14 * 60))).await;
        assert!(
            json["detail"].as_str().unwrap().contains("14 minutes"),
            "{json}"
        );

        // A lock that has already lifted cannot report a negative or zero wait.
        let (_, json) = body(locked_out(Duration::seconds(-5))).await;
        assert!(
            json["detail"].as_str().unwrap().contains("1 minute."),
            "{json}"
        );
    }

    #[tokio::test]
    async fn invalid_credentials_is_an_ordinary_401_with_a_useful_message() {
        let (status, json) = body(invalid_credentials()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["type"], "urn:atlas:error:unauthorized");
        assert_eq!(json["detail"], "Invalid username or password.");
        // ...and it says nothing about which half was wrong.
        let rendered = json.to_string().to_lowercase();
        assert!(!rendered.contains("no such user"), "{rendered}");
        assert!(!rendered.contains("deactivated"), "{rendered}");
    }

    #[tokio::test]
    async fn every_auth_problem_is_still_problem_json() {
        for problem in [
            password_change_required(),
            locked_out(Duration::minutes(1)),
            invalid_credentials(),
        ] {
            let response = problem.into_response();
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/problem+json"
            );
        }
    }
}
