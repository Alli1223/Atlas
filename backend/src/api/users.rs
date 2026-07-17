//! `/api/v1/users` — administration. Admin only, by the extractor in every
//! signature.

use axum::extract::{Path, State};
use axum::response::Json;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::api::serde_ext::double_option;
use crate::auth::events::{self, Kind};
use crate::auth::extract::{ClientInfo, RequireAdmin};
use crate::auth::role::Role;
use crate::auth::user::{NewUser, UserDto, UserPatch};
use crate::auth::{now, password, session, user};
use crate::error::{AppError, AppResult, Problem};

/// Longest accepted username, in characters.
const MAX_USERNAME: usize = 64;

/// Longest accepted display name, in characters.
const MAX_DISPLAY_NAME: usize = 128;

/// Longest accepted email, in characters.
const MAX_EMAIL: usize = 254;

/// The body of `POST /users`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateUserRequest {
    /// The login name. Unique, case-insensitively.
    pub username: String,
    /// Optional email address.
    #[serde(default)]
    pub email: Option<String>,
    /// What the UI shows. Defaults to the username.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The initial password. Must satisfy the policy.
    pub password: String,
    /// Instance-wide role.
    pub role: Role,
    /// Whether the account must change its password on first sign-in.
    ///
    /// Defaults to **true**: an admin who types a password for someone else
    /// knows that password, so the account is not really theirs until they have
    /// replaced it.
    #[serde(default = "default_must_change_password")]
    pub must_change_password: bool,
}

fn default_must_change_password() -> bool {
    true
}

/// The body of `PATCH /users/{id}`.
///
/// `Option<Option<T>>` on `email` and `avatarUrl` is what distinguishes the
/// three things a PATCH body can say: absent (leave it), `null` (clear it), and
/// a value (set it). A plain `Option` conflates the first two, so clearing an
/// email would be impossible.
#[allow(clippy::option_option)]
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateUserRequest {
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub email: Option<Option<String>>,
    /// The display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Absent leaves it, `null` clears it, a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub avatar_url: Option<Option<String>>,
    /// The role.
    #[serde(default)]
    pub role: Option<Role>,
    /// Whether the account can sign in. Setting this to `false` also revokes
    /// every session the account holds.
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Whether the account must change its password before doing anything.
    #[serde(default)]
    pub must_change_password: Option<bool>,
}

/// Every user.
#[utoipa::path(
    get,
    path = "/users",
    tag = "users",
    responses(
        (status = 200, description = "Every user, ordered for display", body = Vec<UserDto>),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
    )
)]
async fn list_users(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> AppResult<Json<Vec<UserDto>>> {
    let users = user::list(&state.db).await?;
    Ok(Json(users.iter().map(UserDto::from).collect()))
}

/// One user.
#[utoipa::path(
    get,
    path = "/users/{id}",
    tag = "users",
    params(("id" = String, Path, description = "The user's id")),
    responses(
        (status = 200, description = "The user", body = UserDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such user", body = Problem),
    )
)]
async fn get_user(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<String>,
) -> AppResult<Json<UserDto>> {
    let found = user::find_by_id(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(UserDto::from(&found)))
}

/// Creates a user.
#[utoipa::path(
    post,
    path = "/users",
    tag = "users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "Created", body = UserDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 409, description = "The username or email is taken", body = Problem),
        (status = 422, description = "The request or the password is invalid", body = Problem),
    )
)]
async fn create_user(
    State(state): State<AppState>,
    admin: RequireAdmin,
    ClientInfo(client): ClientInfo,
    Json(body): Json<CreateUserRequest>,
) -> AppResult<(axum::http::StatusCode, Json<UserDto>)> {
    let username = validate_username(&body.username)?;
    let email = body.email.as_deref().map(validate_email).transpose()?;
    let display_name = match &body.display_name {
        Some(name) => validate_display_name(name)?,
        None => username.clone(),
    };

    // The same policy every password goes through, including the `Admin` rule.
    password::validate(&body.password, &username)?;

    let password_hash = password::hash(body.password).await?;
    let now = now();

    let mut tx = state.db.begin_write().await?;

    // Checked inside the transaction so the check and the insert cannot be
    // separated by another writer. The UNIQUE index is the real guarantee; this
    // exists to turn its 500 into a 409 with a message that says what to fix.
    if user::username_taken(&mut tx, &username, None).await? {
        return Err(AppError::Conflict(format!(
            "The username {username:?} is already taken."
        )));
    }
    if let Some(email) = &email
        && user::email_taken(&mut tx, email, None).await?
    {
        return Err(AppError::Conflict(format!(
            "The email {email:?} is already in use."
        )));
    }

    let created = user::insert(
        &mut tx,
        &NewUser {
            username,
            email,
            display_name,
            password_hash,
            role: body.role,
            must_change_password: body.must_change_password,
        },
        now,
    )
    .await?;

    tx.commit().await?;

    events::record(
        &state.db,
        Kind::UserCreated,
        Some(admin.0.id()),
        &client,
        Some(&format!(
            "created user {:?} ({}) as {}",
            created.username, created.id, created.role
        )),
        now,
    )
    .await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(UserDto::from(&created)),
    ))
}

/// Edits a user.
#[utoipa::path(
    patch,
    path = "/users/{id}",
    tag = "users",
    params(("id" = String, Path, description = "The user's id")),
    request_body = UpdateUserRequest,
    responses(
        (status = 200, description = "Updated", body = UserDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 409, description = "The email is taken, or this would remove the last admin", body = Problem),
        (status = 422, description = "The request is invalid", body = Problem),
    )
)]
async fn update_user(
    State(state): State<AppState>,
    admin: RequireAdmin,
    ClientInfo(client): ClientInfo,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> AppResult<Json<UserDto>> {
    let patch = UserPatch {
        email: body
            .email
            .map(|email| email.as_deref().map(validate_email).transpose())
            .transpose()?,
        display_name: body
            .display_name
            .as_deref()
            .map(validate_display_name)
            .transpose()?,
        avatar_url: body.avatar_url,
        role: body.role,
        is_active: body.is_active,
        must_change_password: body.must_change_password,
    };

    if patch.is_empty() {
        return Err(AppError::Validation(
            "The request changed nothing. Send at least one field.".to_owned(),
        ));
    }

    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = user::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(email) = patch.email.as_ref().and_then(Option::as_ref)
        && user::email_taken(&mut tx, email, Some(&id)).await?
    {
        return Err(AppError::Conflict(format!(
            "The email {email:?} is already in use."
        )));
    }

    // The last-admin guard, checked *after* the write inside the transaction so
    // it accounts for the change itself rather than trying to predict it. An
    // instance with no active admin cannot be repaired through the API — there
    // is nobody left who can promote anyone — so this refusal is the difference
    // between "no" and "restore from a backup".
    let demoting = patch.role.is_some_and(|role| role != Role::Admin);
    let deactivating = patch.is_active == Some(false);

    if target.role == Role::Admin && target.is_active && (demoting || deactivating) {
        // The self-lockout case is worth its own message: an admin removing
        // their own access is nearly always a mistake, and "the last admin"
        // does not describe it when there are others.
        if target.id == admin.0.id() && deactivating {
            return Err(AppError::Conflict(
                "You cannot deactivate your own account. Ask another admin to do it.".to_owned(),
            ));
        }
        if user::active_admin_count(&mut tx).await? <= 1 {
            return Err(AppError::Conflict(
                "This is the only active admin. Promote another admin first, or nobody will be \
                 able to administer this instance."
                    .to_owned(),
            ));
        }
    }

    user::apply_patch(&mut tx, &id, &patch, now).await?;

    // Deactivation has to take effect now, not when the session happens to
    // expire. The authenticate layer also refuses inactive users on every
    // request, so this is the second of two independent guards.
    if deactivating {
        session::delete_all_for_user(&mut tx, &id).await?;
    }

    let updated = user::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    tx.commit().await?;

    events::record(
        &state.db,
        if deactivating {
            Kind::UserDeactivated
        } else {
            Kind::UserUpdated
        },
        Some(admin.0.id()),
        &client,
        Some(&format!(
            "updated user {:?} ({})",
            updated.username, updated.id
        )),
        now,
    )
    .await;

    Ok(Json(UserDto::from(&updated)))
}

/// Deactivates a user.
///
/// There is no delete, and there never will be. Cards, comments, worklogs and
/// every history row reference their author permanently; removing the row would
/// either dangle those references or cascade away the history that made them
/// worth keeping. Deactivation is what "delete this user" means here.
#[utoipa::path(
    post,
    path = "/users/{id}/deactivate",
    tag = "users",
    params(("id" = String, Path, description = "The user's id")),
    responses(
        (status = 200, description = "Deactivated; every session for the account is revoked", body = UserDto),
        (status = 401, description = "Not signed in", body = Problem),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 409, description = "This is you, or the last active admin", body = Problem),
    )
)]
async fn deactivate_user(
    State(state): State<AppState>,
    admin: RequireAdmin,
    ClientInfo(client): ClientInfo,
    Path(id): Path<String>,
) -> AppResult<Json<UserDto>> {
    let now = now();
    let mut tx = state.db.begin_write().await?;

    let target = user::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    if target.id == admin.0.id() {
        return Err(AppError::Conflict(
            "You cannot deactivate your own account. Ask another admin to do it.".to_owned(),
        ));
    }

    if target.role == Role::Admin
        && target.is_active
        && user::active_admin_count(&mut tx).await? <= 1
    {
        return Err(AppError::Conflict(
            "This is the only active admin. Promote another admin first, or nobody will be able \
             to administer this instance."
                .to_owned(),
        ));
    }

    user::deactivate(&mut tx, &id, now).await?;
    let revoked = session::delete_all_for_user(&mut tx, &id).await?;

    let updated = user::find_by_id_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    tx.commit().await?;

    events::record(
        &state.db,
        Kind::UserDeactivated,
        Some(admin.0.id()),
        &client,
        Some(&format!(
            "deactivated user {:?} ({}); {revoked} session(s) revoked",
            updated.username, updated.id
        )),
        now,
    )
    .await;

    Ok(Json(UserDto::from(&updated)))
}

fn validate_username(username: &str) -> AppResult<String> {
    let username = username.trim();

    if username.is_empty() {
        return Err(AppError::Validation(
            "Username must not be empty.".to_owned(),
        ));
    }
    if username.chars().count() > MAX_USERNAME {
        return Err(AppError::Validation(format!(
            "Username must be at most {MAX_USERNAME} characters long."
        )));
    }
    // No interior whitespace and no control characters: `ala stair` and
    // `alastair` are two accounts no human can reliably tell apart, and an
    // embedded newline is a log-injection vector besides.
    if username
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(AppError::Validation(
            "Username must not contain spaces or control characters.".to_owned(),
        ));
    }

    if username.chars().any(is_invisible_or_bidi) {
        return Err(AppError::Validation(
            "Username must not contain invisible or text-direction characters.".to_owned(),
        ));
    }

    Ok(username.to_owned())
}

/// Whether `c` is invisible or can reorder the text around it.
///
/// `char::is_control` is **not** enough on its own, and the gap is the whole
/// reason this function exists: it covers only the C0/C1 control codes (Unicode
/// category `Cc`), while the characters that make one username render as another
/// are category `Cf` — formatting characters that are not "control" characters
/// by that definition and sail straight through.
///
/// The attack is concrete. `alast\u{202e}air` carries a RIGHT-TO-LEFT OVERRIDE:
/// it is a different string from `alastair`, so it is a different account and
/// the `UNIQUE` index is perfectly happy — but in the member list, in a mention,
/// and in an audit log it can be made to *look* like the real one. The
/// zero-width characters are the same trick without the reordering: `alast\u{200b}air`
/// is simply invisible.
///
/// Listed explicitly rather than pulled from a Unicode-category crate: this is
/// the entire set that matters for a single-line identifier, it is stable, and a
/// dependency to answer one question about ~15 code points is not worth its
/// supply chain.
fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c,
        // Zero-width space, non-joiner, joiner.
        '\u{200b}'..='\u{200d}'
        // LEFT-TO-RIGHT MARK, RIGHT-TO-LEFT MARK.
        | '\u{200e}' | '\u{200f}'
        // The bidi embedding/override block: LRE, RLE, PDF, LRO, RLO.
        | '\u{202a}'..='\u{202e}'
        // The bidi isolate block: LRI, RLI, FSI, PDI.
        | '\u{2066}'..='\u{2069}'
        // WORD JOINER.
        | '\u{2060}'
        // ZERO WIDTH NO-BREAK SPACE / BOM.
        | '\u{feff}'
    )
}

fn validate_display_name(name: &str) -> AppResult<String> {
    let name = name.trim();

    if name.is_empty() {
        return Err(AppError::Validation(
            "Display name must not be empty.".to_owned(),
        ));
    }
    if name.chars().count() > MAX_DISPLAY_NAME {
        return Err(AppError::Validation(format!(
            "Display name must be at most {MAX_DISPLAY_NAME} characters long."
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "Display name must not contain control characters.".to_owned(),
        ));
    }

    Ok(name.to_owned())
}

/// Checks an email just enough to catch a typo, and no further.
///
/// Deliberately not a grammar: RFC 5322 admits addresses no validator agrees on,
/// every regex anyone writes for this rejects somebody's real address, and Atlas
/// does not need the address to be deliverable — it needs it to be a plausible
/// identifier and to be unique. The real check is a confirmation email, which
/// Atlas does not send.
fn validate_email(email: &str) -> AppResult<String> {
    let email = email.trim();

    if email.chars().count() > MAX_EMAIL {
        return Err(AppError::Validation(format!(
            "Email must be at most {MAX_EMAIL} characters long."
        )));
    }

    let looks_like_an_address = match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && domain.contains('.')
                && !domain.ends_with('.')
        }
        None => false,
    };

    if !looks_like_an_address || email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::Validation(format!(
            "{email:?} does not look like an email address."
        )));
    }

    Ok(email.to_owned())
}

/// The `/users` routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        // axum 0.8: `{id}`, never `:id` — the 0.7 syntax is a runtime panic.
        .routes(routes!(list_users, create_user))
        .routes(routes!(get_user, update_user))
        .routes(routes!(deactivate_user))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_trimmed_and_bounded() {
        assert_eq!(validate_username("  alastair  ").unwrap(), "alastair");
        assert!(validate_username("").is_err());
        assert!(validate_username("   ").is_err());
        assert!(validate_username(&"a".repeat(MAX_USERNAME + 1)).is_err());
        assert!(validate_username(&"a".repeat(MAX_USERNAME)).is_ok());
    }

    #[test]
    fn usernames_reject_interior_whitespace_and_control_characters() {
        // "ala stair" and "alastair" are different strings that look like typos
        // of each other; two accounts no human can reliably tell apart is the
        // thing to prevent.
        assert!(validate_username("ala stair").is_err());
        assert!(
            validate_username("ala\u{a0}stair").is_err(),
            "non-breaking space"
        );
        assert!(validate_username("ala\nstair").is_err());
        assert!(validate_username("ala\tstair").is_err());
        // A NUL is not whitespace, so `trim` does not remove it — it has to be
        // rejected outright wherever it sits.
        assert!(validate_username("alastair\u{0}").is_err());
        assert!(validate_username("\u{0}alastair").is_err());
    }

    #[test]
    fn usernames_reject_invisible_and_bidi_characters() {
        // These are NOT caught by `is_control` — they are Unicode category Cf,
        // not Cc — and each of them makes a distinct username that renders as
        // an existing one. A convincing "alastair" that is not alastair is the
        // whole attack.
        assert!(
            validate_username("alast\u{202e}air").is_err(),
            "right-to-left override"
        );
        assert!(
            validate_username("alast\u{200b}air").is_err(),
            "zero-width space"
        );
        assert!(
            validate_username("alast\u{200d}air").is_err(),
            "zero-width joiner"
        );
        assert!(
            validate_username("alast\u{2066}air").is_err(),
            "bidi isolate"
        );
        assert!(validate_username("\u{feff}alastair").is_err(), "BOM");

        // The characters the check exists to distinguish itself from: ordinary
        // non-ASCII text is fine, and rejecting it would be a bug.
        assert!(validate_username("アラステア").is_ok());
        assert!(validate_username("alastair-rayner_1").is_ok());
        assert!(validate_username("émile").is_ok());
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_rejected() {
        // Deliberate, and the reason the test above is about *interior*
        // whitespace: a stray trailing space or newline from a form field is a
        // client artefact, not a request for a different username.
        assert_eq!(validate_username("alastair\n").unwrap(), "alastair");
        assert_eq!(validate_username("\talastair ").unwrap(), "alastair");
        assert_eq!(validate_username("alastair\u{a0}").unwrap(), "alastair");
        // ...and trimming to nothing is still empty, not a username.
        assert!(validate_username("\n\t ").is_err());
    }

    #[test]
    fn display_names_may_contain_spaces_but_not_control_characters() {
        assert_eq!(
            validate_display_name("  Alastair Rayner  ").unwrap(),
            "Alastair Rayner"
        );
        assert!(validate_display_name("Alastair\nRayner").is_err());
        assert!(validate_display_name("").is_err());
        assert!(validate_display_name(&"a".repeat(MAX_DISPLAY_NAME + 1)).is_err());
    }

    #[test]
    fn emails_are_checked_for_plausibility_only() {
        assert_eq!(
            validate_email(" alastair@example.com ").unwrap(),
            "alastair@example.com"
        );
        assert!(validate_email("a+tag@sub.example.co.uk").is_ok());

        assert!(validate_email("not-an-address").is_err());
        assert!(validate_email("@example.com").is_err());
        assert!(validate_email("alastair@").is_err());
        assert!(validate_email("alastair@localhost").is_err(), "no dot");
        assert!(validate_email("alastair@example.").is_err());
        assert!(validate_email("alastair @example.com").is_err());
        assert!(validate_email("alastair@example.com\nBcc: x").is_err());
        assert!(validate_email(&format!("{}@example.com", "a".repeat(MAX_EMAIL))).is_err());
    }

    #[test]
    fn create_defaults_a_new_account_to_forced_reset() {
        // An admin who types someone else's password knows it. The account is
        // not really theirs until they have replaced it.
        let body: CreateUserRequest = serde_json::from_str(
            r#"{"username":"x","password":"a perfectly fine passphrase","role":"member"}"#,
        )
        .unwrap();
        assert!(body.must_change_password);
        assert_eq!(body.role, Role::Member);
        assert!(body.display_name.is_none());

        // ...and it can still be turned off deliberately.
        let body: CreateUserRequest = serde_json::from_str(
            r#"{"username":"x","password":"a perfectly fine passphrase","role":"member","mustChangePassword":false}"#,
        )
        .unwrap();
        assert!(!body.must_change_password);
    }

    #[test]
    fn a_patch_distinguishes_absent_from_null() {
        // The reason for Option<Option<_>>: these two bodies mean different
        // things, and a plain Option cannot tell them apart.
        let absent: UpdateUserRequest = serde_json::from_str(r#"{"displayName":"X"}"#).unwrap();
        assert_eq!(absent.email, None, "absent means leave it alone");

        let null: UpdateUserRequest = serde_json::from_str(r#"{"email":null}"#).unwrap();
        assert_eq!(null.email, Some(None), "null means clear it");

        let set: UpdateUserRequest = serde_json::from_str(r#"{"email":"a@b.test"}"#).unwrap();
        assert_eq!(set.email, Some(Some("a@b.test".to_owned())));
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        // deny_unknown_fields: a typo'd `mustChangePasword` must be a 422, not a
        // silently ignored security setting.
        let err = serde_json::from_str::<UpdateUserRequest>(r#"{"mustChangePasword":true}"#);
        assert!(err.is_err());
        let err = serde_json::from_str::<CreateUserRequest>(
            r#"{"username":"x","password":"y","role":"member","isAdmin":true}"#,
        );
        assert!(err.is_err());
    }
}
