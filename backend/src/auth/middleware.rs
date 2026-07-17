//! The layers that turn a cookie into a [`CurrentUser`], and the two gates that
//! run before any handler sees the request.

use std::sync::Arc;

use axum::extract::{OriginalUri, Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;

use crate::api::AppState;
use crate::auth::extract::CurrentUser;
use crate::auth::{now, problem, session, user};
use crate::config::{AppEnv, Config};
use crate::error::AppError;

/// The routes that stay open to an account with `must_change_password` set.
///
/// Exactly three, and each is here because the flow is impossible without it:
///
/// - **change-password** — the way out. Blocking it would brick the account.
/// - **logout** — a user who cannot change their password must still be able to
///   leave, and revoking a session is not something the gate protects.
/// - **me** — the SPA fetches this on load to decide what to render. Blocking it
///   would mean the client cannot discover *why* it is being blocked, and the
///   only thing it could show is a generic error page.
///
/// Full paths, matched against [`OriginalUri`] — see [`is_allowlisted`].
const FORCED_RESET_ALLOWLIST: &[(Method, &str)] = &[
    (Method::POST, "/api/v1/auth/change-password"),
    (Method::POST, "/api/v1/auth/logout"),
    (Method::GET, "/api/v1/auth/me"),
];

/// Whether the session cookie should carry `Secure`.
///
/// `true` in prod, `false` in dev. It cannot simply always be `true`: dev serves
/// plain HTTP on localhost, where a `Secure` cookie is silently discarded by the
/// browser — which presents as "login returns 200 and then I am still logged
/// out", with nothing in any log.
pub fn cookie_secure(config: &Config) -> bool {
    config.env == AppEnv::Prod
}

/// Authenticates the request, then enforces the forced-reset gate.
///
/// # Why these are one layer
///
/// The gate needs the user, and loading the user is what authentication does.
/// Splitting them would mean two session lookups and two user loads per request,
/// or a second extension to pass one to the other.
///
/// # Why the gate is a layer at all
///
/// Because a per-handler check is a thing a new handler can forget, and the
/// failure mode of forgetting is silent: the route simply works for an account
/// that should be locked to the reset screen. Here, a route added anywhere under
/// `/api/v1` is gated by default and has to be *deliberately* added to
/// [`FORCED_RESET_ALLOWLIST`] to escape.
///
/// # Unauthenticated requests pass through
///
/// This layer never rejects for want of a session. It attaches a [`CurrentUser`]
/// when there is one and does nothing when there is not; deciding whether a
/// route needs authentication is the extractor's job, in the handler's signature,
/// where it is visible. That is what keeps `POST /auth/login` working while it
/// sits under the same layer.
pub async fn authenticate(
    State(state): State<AppState>,
    jar: CookieJar,
    original_uri: OriginalUri,
    mut request: Request,
    next: Next,
) -> Response {
    let now = now();

    let Some(token) = jar.get(session::COOKIE_NAME).map(|c| c.value().to_owned()) else {
        return next.run(request).await;
    };

    let session = match session::load(&state.db, &token, now).await {
        Ok(Some(session)) => session,
        // No session, or an expired one: carry on unauthenticated.
        Ok(None) => return next.run(request).await,
        Err(err) => return err.into_response(),
    };

    let user = match user::find_by_id(&state.db, &session.user_id).await {
        Ok(Some(user)) => user,
        // The session points at nobody. Cannot happen while the FK holds, so it
        // is worth a loud log rather than a silent pass-through.
        Ok(None) => {
            tracing::error!(
                session_id = %session.id,
                user_id = %session.user_id,
                "session references a user that does not exist"
            );
            return next.run(request).await;
        }
        Err(err) => return err.into_response(),
    };

    // A deactivated account's sessions stop working on the next request, without
    // waiting for the deactivate handler's own revocation to have succeeded.
    if !user.is_active {
        return next.run(request).await;
    }

    // Slide the idle window. Throttled inside `touch`, so this is not a write
    // per request.
    let mut session = session;
    match session::touch(&state.db, &session, now).await {
        Ok(last_seen_at) => session.last_seen_at = last_seen_at,
        // A failed refresh is not a failed request: the session is still valid
        // right now, and the worst case is that it expires slightly early.
        Err(err) => tracing::warn!(error = ?err, "failed to refresh the session idle window"),
    }

    let must_change_password = user.must_change_password;

    request.extensions_mut().insert(CurrentUser {
        user: Arc::new(user),
        session: Arc::new(session),
    });

    if must_change_password && !is_allowlisted(request.method(), original_uri.path()) {
        return problem::password_change_required().into_response();
    }

    next.run(request).await
}

/// Whether a request is one of the three the forced-reset gate lets past.
///
/// The path comes from [`OriginalUri`], not `request.uri()`. `Router::nest`
/// strips the mount prefix before the inner router sees the request, so inside
/// the `/api/v1` nest `request.uri().path()` is `/auth/me` — and matching that
/// against `/api/v1/auth/me` would allowlist nothing, locking the account out of
/// its own password-change screen. `OriginalUri` is the untouched path.
fn is_allowlisted(method: &Method, path: &str) -> bool {
    FORCED_RESET_ALLOWLIST
        .iter()
        .any(|(allowed_method, allowed_path)| allowed_method == method && *allowed_path == path)
}

/// Rejects state-changing requests whose `Origin` is not ours.
///
/// # Why, given `SameSite=Lax` already exists
///
/// Defence in depth, and the two do not fail in the same way. `SameSite=Lax` is
/// the primary control — the browser simply does not attach the cookie to a
/// cross-site `POST`, so the classic form-submission CSRF cannot happen. But it
/// is a property of the *browser*, not of Atlas: it depends on the browser
/// implementing Lax correctly, on it not being relaxed by a future spec change,
/// and on nobody ever deciding Atlas needs `SameSite=None` for an embed. The
/// origin check is a property of Atlas, and it holds when any of that changes.
///
/// # Why an origin check rather than a CSRF token
///
/// A double-submit token means a token endpoint, a cookie, a header, and a
/// synchroniser in the SPA — roughly a hundred lines of machinery to re-derive
/// what the browser already tells us for free in a header it will not let a
/// cross-site page forge. `Origin` is unforgeable from JavaScript: it is on the
/// forbidden-header list. Same protection, no state.
///
/// # What is checked
///
/// - Safe methods (`GET`, `HEAD`, `OPTIONS`) are not checked. They are not
///   supposed to change state, and preflights must get through to `CorsLayer`.
/// - `Origin`, when present, must be a configured CORS origin **or** must match
///   the request's own `Host`. The `Host` case is the single-binary deployment
///   (Phase 20), where the SPA and the API share an origin and nobody has
///   configured CORS at all — without it, production would reject every write.
/// - `Referer` is the fallback when `Origin` is absent, checked the same way.
/// - When neither header is present, the request passes. Every browser has sent
///   `Origin` on cross-origin state-changing requests for years, so "no `Origin`"
///   means a non-browser client — curl, a script, an integration — which is not
///   what CSRF is. Rejecting it would break every API client to protect against
///   an attack that requires a browser.
pub async fn verify_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }

    let headers = request.headers();

    let claimed = header_str(headers, header::ORIGIN)
        // A Referer is a full URL; only its origin is comparable.
        .or_else(|| header_str(headers, header::REFERER).and_then(|r| origin_of(&r)));

    let Some(claimed) = claimed else {
        return next.run(request).await;
    };

    let host = header_str(headers, header::HOST);

    if !origin_allowed(&claimed, host.as_deref(), &state.config) {
        tracing::warn!(
            origin = %claimed,
            method = %request.method(),
            "rejected a state-changing request from an unrecognised origin"
        );
        return AppError::Forbidden.into_response();
    }

    next.run(request).await
}

/// Whether `origin` may make a state-changing request.
fn origin_allowed(origin: &str, host: Option<&str>, config: &Config) -> bool {
    // `*` means the operator has already accepted any origin. Cookie auth does
    // not work cross-origin under a wildcard anyway — the CORS spec forbids
    // credentials with `*` — so there is nothing left here to defend.
    if config.cors_allows_any_origin() {
        return true;
    }

    if config
        .cors_origins()
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(origin))
    {
        return true;
    }

    // Same-origin: the SPA served from the same host as the API. Compared on
    // authority alone, ignoring the scheme, because behind a TLS-terminating
    // proxy the browser says `https://atlas.example.com` while `Host` says
    // `atlas.example.com` and the connection Atlas sees is plain HTTP. A
    // cross-site attacker's Origin is their own host and matches neither.
    match (authority_of(origin), host) {
        (Some(origin_authority), Some(host)) => origin_authority.eq_ignore_ascii_case(host),
        _ => false,
    }
}

/// The `scheme://authority` prefix of a URL, for comparing a `Referer` to an
/// `Origin`.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// The `host[:port]` of an origin.
fn authority_of(origin: &str) -> Option<&str> {
    let (_scheme, authority) = origin.split_once("://")?;
    (!authority.is_empty()).then_some(authority)
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(origins: &str) -> Config {
        Config {
            cors_allowed_origins: origins.to_owned(),
            ..Config::default()
        }
    }

    #[test]
    fn the_cookie_is_secure_in_prod_and_not_in_dev() {
        assert!(!cookie_secure(&Config {
            env: AppEnv::Dev,
            ..Config::default()
        }));
        assert!(cookie_secure(&Config {
            env: AppEnv::Prod,
            master_key: Some(crate::config::SecretString::new("k")),
            ..Config::default()
        }));
    }

    #[test]
    fn only_the_three_documented_routes_escape_the_forced_reset_gate() {
        assert!(is_allowlisted(
            &Method::POST,
            "/api/v1/auth/change-password"
        ));
        assert!(is_allowlisted(&Method::POST, "/api/v1/auth/logout"));
        assert!(is_allowlisted(&Method::GET, "/api/v1/auth/me"));

        // Everything else is gated, including neighbours that look similar.
        assert!(!is_allowlisted(&Method::GET, "/api/v1/users"));
        assert!(!is_allowlisted(&Method::POST, "/api/v1/auth/login"));
        assert!(!is_allowlisted(&Method::GET, "/api/v1/auth/sessions"));
    }

    #[test]
    fn the_allowlist_is_matched_on_method_as_well_as_path() {
        // GET /auth/change-password is not the change-password call, and must
        // not be a hole in the gate.
        assert!(!is_allowlisted(
            &Method::GET,
            "/api/v1/auth/change-password"
        ));
        assert!(!is_allowlisted(&Method::DELETE, "/api/v1/auth/logout"));
        assert!(!is_allowlisted(&Method::POST, "/api/v1/auth/me"));
    }

    #[test]
    fn the_allowlist_matches_the_full_path_not_a_suffix() {
        // If the gate ever matched on the nest-stripped path, or on a suffix,
        // an attacker-chosen prefix would open it.
        assert!(!is_allowlisted(&Method::GET, "/auth/me"));
        assert!(!is_allowlisted(&Method::GET, "/api/v1/auth/me/extra"));
        assert!(!is_allowlisted(&Method::GET, "/evil/api/v1/auth/me"));
    }

    #[test]
    fn a_configured_origin_is_allowed() {
        let config = config("http://localhost:5173, https://atlas.example.com");
        assert!(origin_allowed("http://localhost:5173", None, &config));
        assert!(origin_allowed("https://atlas.example.com", None, &config));
    }

    #[test]
    fn an_unconfigured_origin_is_rejected() {
        let config = config("http://localhost:5173");
        assert!(!origin_allowed("https://evil.test", None, &config));
        assert!(!origin_allowed(
            "https://evil.test",
            Some("localhost:5173"),
            &config
        ));
        // A prefix or suffix of a real origin is not a real origin.
        assert!(!origin_allowed(
            "http://localhost:5173.evil.test",
            None,
            &config
        ));
        assert!(!origin_allowed(
            "http://evil.test/http://localhost:5173",
            None,
            &config
        ));
    }

    #[test]
    fn a_same_origin_request_is_allowed_even_with_no_cors_configuration() {
        // The single-binary deploy: the SPA and the API share a host, CORS is
        // irrelevant, and nobody has set ATLAS_CORS_ALLOWED_ORIGINS. Without
        // this branch, production rejects every write.
        let config = config("http://localhost:5173");
        assert!(origin_allowed(
            "https://atlas.example.com",
            Some("atlas.example.com"),
            &config
        ));
        // Including the scheme mismatch a TLS-terminating proxy produces.
        assert!(origin_allowed(
            "http://atlas.example.com",
            Some("atlas.example.com"),
            &config
        ));
        // And with a port.
        assert!(origin_allowed(
            "http://atlas.example.com:8080",
            Some("atlas.example.com:8080"),
            &config
        ));
    }

    #[test]
    fn a_host_that_merely_resembles_the_origin_is_not_the_same_origin() {
        let config = config("http://localhost:5173");
        assert!(!origin_allowed(
            "https://atlas.example.com.evil.test",
            Some("atlas.example.com"),
            &config
        ));
        assert!(!origin_allowed(
            "https://atlas.example.com:9999",
            Some("atlas.example.com"),
            &config
        ));
    }

    #[test]
    fn a_wildcard_cors_configuration_disables_the_check() {
        let config = config("*");
        assert!(origin_allowed("https://evil.test", None, &config));
    }

    #[test]
    fn a_referer_is_reduced_to_its_origin() {
        assert_eq!(
            origin_of("https://atlas.example.com/board/1?q=x#frag"),
            Some("https://atlas.example.com".to_owned())
        );
        assert_eq!(
            origin_of("http://localhost:5173/"),
            Some("http://localhost:5173".to_owned())
        );
        assert_eq!(
            origin_of("http://localhost:5173"),
            Some("http://localhost:5173".to_owned())
        );
        // Not a URL at all.
        assert_eq!(origin_of("localhost:5173"), None);
        assert_eq!(origin_of("https://"), None);
        assert_eq!(origin_of(""), None);
    }

    #[test]
    fn the_authority_is_extracted_without_the_scheme() {
        assert_eq!(
            authority_of("https://atlas.example.com"),
            Some("atlas.example.com")
        );
        assert_eq!(
            authority_of("http://localhost:5173"),
            Some("localhost:5173")
        );
        assert_eq!(authority_of("null"), None);
        assert_eq!(authority_of(""), None);
    }

    #[test]
    fn the_opaque_null_origin_is_rejected() {
        // Sandboxed iframes and some redirects send `Origin: null`. It is not
        // ours, and treating it as absent would let it through.
        let config = config("http://localhost:5173");
        assert!(!origin_allowed("null", Some("localhost:5173"), &config));
        assert!(!origin_allowed("null", None, &config));
    }
}
