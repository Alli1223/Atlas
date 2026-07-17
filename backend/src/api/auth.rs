//! `/api/v1/auth` — login, logout, me, change-password, session list and revoke.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::auth::events::{self, Client, Kind};
use crate::auth::extract::{ClientInfo, CurrentUser};
use crate::auth::middleware::cookie_secure;
use crate::auth::session::SessionDto;
use crate::auth::user::UserDto;
use crate::auth::{lockout, now, password, problem, session, user};
use crate::error::{AppError, AppResult, Problem};

/// The failure modes of a credential check.
///
/// A local enum rather than new [`AppError`] variants, because these two are the
/// only routes in Atlas that can produce them and they need response shapes the
/// shared taxonomy does not have (see [`crate::auth::problem`]). Everything else
/// still flows through `AppError` via the `From` impl below, so `?` works as
/// usual and this stays a thin wrapper rather than a second taxonomy.
#[derive(Debug)]
enum AuthFailure {
    /// Wrong password, unknown username, or a deactivated account — 401, and
    /// deliberately the same document for all three.
    InvalidCredentials,
    /// The username or the address is locked out — 429.
    LockedOut(chrono::Duration),
    /// Anything else, rendered by `AppError`.
    App(AppError),
}

impl From<AppError> for AuthFailure {
    fn from(err: AppError) -> Self {
        Self::App(err)
    }
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidCredentials => problem::invalid_credentials().into_response(),
            Self::LockedOut(retry_after) => problem::locked_out(retry_after).into_response(),
            Self::App(err) => err.into_response(),
        }
    }
}

/// Credentials for `POST /auth/login`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginRequest {
    /// The username. Matched case-insensitively.
    pub username: String,
    /// The password.
    pub password: String,
}

/// The body of `POST /auth/change-password`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangePasswordRequest {
    /// The password being replaced. Re-checked even though the caller is already
    /// authenticated: a borrowed unlocked laptop must not be enough to take an
    /// account over permanently.
    pub current_password: String,
    /// The replacement. Must satisfy [`crate::auth::password::validate`].
    pub new_password: String,
}

/// Signs in and sets the session cookie.
///
/// # Constant time with respect to whether the username exists
///
/// The unknown-username path hashes `password` against a throwaway hash before
/// failing (see [`password::verify_dummy`]). Without it, an unknown username
/// returns in microseconds and a known one takes ~50ms of Argon2, and anyone
/// with a stopwatch can enumerate every account in the instance without ever
/// guessing a password.
///
/// The same reasoning is why a deactivated account's password is still verified
/// before the request is refused, and why all three failures return the byte-for-byte
/// identical document.
#[utoipa::path(
    post,
    path = "/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in; the session cookie is set", body = UserDto),
        (status = 401, description = "Invalid username or password", body = Problem),
        (status = 429, description = "Too many failed attempts for this username or address", body = Problem),
    )
)]
async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    ClientInfo(client): ClientInfo,
    Json(body): Json<LoginRequest>,
) -> Result<(CookieJar, Json<UserDto>), AuthFailure> {
    let now = now();
    let username = body.username.trim().to_owned();

    let user_key = lockout::user_key(&username);
    // No address means no per-IP counter — never a shared "unknown" key, which
    // would be one lockout counter for every anonymous request on the instance.
    let ip_key = client.ip.as_deref().map(lockout::ip_key);

    refuse_if_locked(&state, &user_key, ip_key.as_deref(), None, &client, now).await?;

    let found = user::find_by_username(&state.db, &username).await?;

    // Verify in both branches, at the same cost. `found` is checked *after* the
    // hashing, not before it.
    let password_matches = match &found {
        Some(user) => password::verify(body.password, user.password_hash.clone()).await?,
        None => password::verify_dummy(body.password).await?,
    };

    let authenticated = match &found {
        Some(user) if password_matches && user.is_active => user,
        other => {
            let reason = match (other, password_matches) {
                (None, _) => "no such username",
                (Some(_), false) => "wrong password",
                (Some(_), true) => "account is deactivated",
            };
            return Err(reject_login(
                &state,
                &user_key,
                ip_key.as_deref(),
                other.as_ref(),
                &client,
                reason,
            )
            .await);
        }
    };

    // A good login forgives the counters. Otherwise a user who mistyped nine
    // times and then succeeded would still be one typo from a lockout.
    forgive_counters(&state, &user_key, ip_key.as_deref()).await?;

    let issued = session::create(
        &state.db,
        &authenticated.id,
        client.user_agent.as_deref(),
        client.ip.as_deref(),
        now,
    )
    .await?;

    // Bookkeeping: a failure here must not fail a login that worked.
    if let Err(err) = user::touch_last_login(&state.db, &authenticated.id, now).await {
        tracing::warn!(error = ?err, "failed to record last_login_at");
    }

    events::record(
        &state.db,
        Kind::LoginSucceeded,
        Some(&authenticated.id),
        &client,
        None,
        now,
    )
    .await;

    let jar = jar.add(session::cookie(issued.token, cookie_secure(&state.config)));
    Ok((jar, Json(UserDto::from(authenticated))))
}

/// Refuses early if either counter is locked, **before** any Argon2 is spent.
///
/// The ordering is the whole point. A locked key has to be cheap to refuse: if
/// the hash ran first and the lock were consulted afterwards, every refused
/// request would still cost ~50 ms of CPU and 19 MiB on the blocking pool, and
/// the lockout would be a `DoS` amplifier rather than a defence.
///
/// Shared by `login` and `change_password` because they are two doors onto one
/// secret. Guesses arriving at either must spend from the same budget — a
/// separate counter per route would just mean an attacker gets
/// [`lockout::MAX_FAILURES`] guesses *per route*.
async fn refuse_if_locked(
    state: &AppState,
    user_key: &str,
    ip_key: Option<&str>,
    user_id: Option<&str>,
    client: &Client,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), AuthFailure> {
    for key in [Some(user_key), ip_key].into_iter().flatten() {
        if let Some(retry_after) = lockout::locked_for(&state.db, key, now).await? {
            events::record(
                &state.db,
                Kind::LoginLockedOut,
                user_id,
                client,
                Some(&format!("locked key {key}")),
                now,
            )
            .await;
            return Err(AuthFailure::LockedOut(retry_after));
        }
    }
    Ok(())
}

/// Clears both counters after a correct password.
///
/// Otherwise a user who mistypes nine times and then succeeds stays one typo
/// away from a lockout.
async fn forgive_counters(state: &AppState, user_key: &str, ip_key: Option<&str>) -> AppResult<()> {
    for key in [Some(user_key), ip_key].into_iter().flatten() {
        lockout::clear(&state.db, key).await?;
    }
    Ok(())
}

/// Records a failed login against both counters and returns the one response
/// every failure shares.
async fn reject_login(
    state: &AppState,
    user_key: &str,
    ip_key: Option<&str>,
    found: Option<&user::User>,
    client: &Client,
    reason: &str,
) -> AuthFailure {
    let now = now();

    // Counters advance for usernames that do not exist too. Skipping them would
    // be the obvious optimisation and would turn "this username locks out" into
    // a membership oracle, undoing the dummy hash above.
    for key in [Some(user_key), ip_key].into_iter().flatten() {
        match lockout::record_failure(&state.db, key, now).await {
            Ok(attempts) if attempts.locked_until.is_some() => {
                events::record(
                    &state.db,
                    Kind::LockedOut,
                    found.map(|u| u.id.as_str()),
                    client,
                    Some(&format!(
                        "{key} locked after {} failures",
                        attempts.failures
                    )),
                    now,
                )
                .await;
            }
            Ok(_) => {}
            Err(err) => {
                // The counter is a defence, not the gate. If it cannot be
                // written, the login still fails — it just fails uncounted.
                tracing::error!(error = ?err, key, "failed to record a login failure");
            }
        }
    }

    events::record(
        &state.db,
        Kind::LoginFailed,
        found.map(|u| u.id.as_str()),
        client,
        Some(reason),
        now,
    )
    .await;

    AuthFailure::InvalidCredentials
}

/// Signs out, revoking this session.
#[utoipa::path(
    post,
    path = "/auth/logout",
    tag = "auth",
    responses(
        (status = 204, description = "Signed out; the session is revoked and the cookie cleared"),
    )
)]
async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    ClientInfo(client): ClientInfo,
    current: Option<CurrentUser>,
) -> AppResult<(CookieJar, StatusCode)> {
    // `Option<CurrentUser>`: logging out when already logged out is a success,
    // not a 401. The client's goal — "this browser holds no session" — is met
    // either way, and returning an error would leave the SPA unable to recover
    // from a session that expired while the tab was open.
    if let Some(current) = &current {
        session::delete(&state.db, &current.session.id).await?;
        events::record(
            &state.db,
            Kind::LoggedOut,
            Some(current.id()),
            &client,
            None,
            now(),
        )
        .await;
    }

    let jar = jar.add(session::removal_cookie(cookie_secure(&state.config)));
    Ok((jar, StatusCode::NO_CONTENT))
}

/// The signed-in user.
///
/// Reachable while `mustChangePassword` is set — the SPA needs it to know that,
/// and to render the reset screen at all.
#[utoipa::path(
    get,
    path = "/auth/me",
    tag = "auth",
    responses(
        (status = 200, description = "The signed-in user", body = UserDto),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn me(current: CurrentUser) -> Json<UserDto> {
    Json(UserDto::from(current.user.as_ref()))
}

/// Changes the signed-in user's password, and rotates the session.
///
/// # Rate limited, on the same counter as login
///
/// `currentPassword` is re-checked here, and the reason that check exists — a
/// borrowed unlocked laptop must not be enough to take an account over — is
/// precisely the threat model of an attacker who holds the session but not the
/// password. An unlimited number of guesses at it is therefore not a check at
/// all: it is a slower `POST /auth/login` with no lockout.
///
/// So this route spends from the *same* per-username and per-IP counters that
/// `login` does, and consults them before any Argon2 runs. Two counters would be
/// two budgets for guessing one secret, and an attacker would simply alternate
/// between the routes.
///
/// # Rotation
///
/// Every session for the user is revoked and a new one issued, so the cookie the
/// client ends up with is not the one it arrived with. Two reasons:
///
/// - **Fixation.** If an attacker somehow fixed a known session id on the
///   victim's browser, changing the password would otherwise leave that id
///   valid — the credential changes and the thing that grants access does not.
/// - **"Change my password" means "kick everyone else out".** That is what a
///   user believes they are doing, and it is the correct response to the reason
///   they are usually doing it.
#[utoipa::path(
    post,
    path = "/auth/change-password",
    tag = "auth",
    request_body = ChangePasswordRequest,
    responses(
        (status = 200, description = "Password changed; a new session cookie is set", body = UserDto),
        (status = 401, description = "The current password is wrong", body = Problem),
        (status = 422, description = "The new password does not satisfy the policy", body = Problem),
    )
)]
async fn change_password(
    State(state): State<AppState>,
    jar: CookieJar,
    ClientInfo(client): ClientInfo,
    current: CurrentUser,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<(CookieJar, Json<UserDto>), AuthFailure> {
    let now = now();

    // The same counters `login` spends from, keyed on the same username. See
    // `refuse_if_locked`: `current_password` is the same secret as the one
    // `login` checks, so an attacker must not get a fresh budget of guesses at
    // it simply by knocking on a different door.
    let user_key = lockout::user_key(&current.user.username);
    let ip_key = client.ip.as_deref().map(lockout::ip_key);

    refuse_if_locked(
        &state,
        &user_key,
        ip_key.as_deref(),
        Some(current.id()),
        &client,
        now,
    )
    .await?;

    // Re-verify, even though this request is authenticated.
    if !password::verify(body.current_password, current.user.password_hash.clone()).await? {
        return Err(reject_login(
            &state,
            &user_key,
            ip_key.as_deref(),
            Some(current.user.as_ref()),
            &client,
            "wrong current password on change-password",
        )
        .await);
    }

    // The guess was right, so the counters are forgiven — a user who fumbles the
    // old password twice and then gets it right must not be left near a lock.
    // Done here rather than after the write so that the validation failures
    // below (too short, reused, `Admin`) do not leave the counter armed: those
    // are the *new* password being wrong, which is not a guess at the old one.
    forgive_counters(&state, &user_key, ip_key.as_deref()).await?;

    // The policy owns the `Admin` rule, the length floor, the username check and
    // the common-password list.
    password::validate(&body.new_password, &current.user.username).map_err(AuthFailure::App)?;

    // Verified rather than string-compared: the current password is only in hand
    // as a hash for the *rest* of this function, and re-hashing to compare would
    // not work anyway (different salt, different hash).
    if password::verify(
        body.new_password.clone(),
        current.user.password_hash.clone(),
    )
    .await?
    {
        return Err(AuthFailure::App(AppError::Validation(
            "The new password must be different from the current one.".to_owned(),
        )));
    }

    let password_hash = password::hash(body.new_password).await?;

    let mut tx = state.db.begin_write().await.map_err(AppError::from)?;
    user::set_password(&mut tx, current.id(), &password_hash, now)
        .await
        .map_err(AuthFailure::App)?;
    // Every session, including this one. The replacement is issued below.
    session::delete_all_for_user(&mut tx, current.id())
        .await
        .map_err(AuthFailure::App)?;
    tx.commit().await.map_err(AppError::from)?;

    let issued = session::create(
        &state.db,
        current.id(),
        client.user_agent.as_deref(),
        client.ip.as_deref(),
        now,
    )
    .await?;

    events::record(
        &state.db,
        Kind::PasswordChanged,
        Some(current.id()),
        &client,
        Some("password changed; all sessions rotated"),
        now,
    )
    .await;

    // Re-read rather than patching the in-hand copy: `must_change_password` was
    // just cleared by the UPDATE, and the client's next move depends on the DTO
    // saying so.
    let updated = user::find_by_id(&state.db, current.id())
        .await?
        .ok_or(AppError::NotFound)?;

    let jar = jar.add(session::cookie(issued.token, cookie_secure(&state.config)));
    Ok((jar, Json(UserDto::from(&updated))))
}

/// The signed-in user's own sessions.
#[utoipa::path(
    get,
    path = "/auth/sessions",
    tag = "auth",
    responses(
        (status = 200, description = "This user's sessions, newest first", body = Vec<SessionDto>),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn list_sessions(
    State(state): State<AppState>,
    current: CurrentUser,
) -> AppResult<Json<Vec<SessionDto>>> {
    let sessions = session::list_for_user(&state.db, current.id()).await?;
    Ok(Json(
        sessions
            .iter()
            .map(|s| SessionDto::from_session(s, &current.session.id))
            .collect(),
    ))
}

/// Revokes one of the signed-in user's sessions.
///
/// Only ever your own: the id comes from the client, and without the ownership
/// check any user could log any other user out by guessing — or, since ids are
/// listed to their owner, by being told.
#[utoipa::path(
    delete,
    path = "/auth/sessions/{id}",
    tag = "auth",
    params(("id" = String, Path, description = "The session id, as returned by GET /auth/sessions")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 404, description = "No such session for this user", body = Problem),
    )
)]
async fn revoke_session(
    State(state): State<AppState>,
    current: CurrentUser,
    ClientInfo(client): ClientInfo,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    // Someone else's session id is a 404, not a 403: "that is not yours" would
    // confirm the id is real.
    if !session::delete_owned(&state.db, &id, current.id()).await? {
        return Err(AppError::NotFound);
    }

    events::record(
        &state.db,
        Kind::SessionRevoked,
        Some(current.id()),
        &client,
        Some("session revoked by its owner"),
        now(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

/// The `/auth` routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(change_password))
        .routes(routes!(list_sessions))
        .routes(routes!(revoke_session))
}
