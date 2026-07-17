//! Extractors: who is making this request, and are they allowed to?

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, OptionalFromRequestParts};
use axum::http::header;
use axum::http::request::Parts;

use crate::auth::events::Client;
use crate::auth::role::Role;
use crate::auth::session::Session;
use crate::auth::user::User;
use crate::error::AppError;

/// The authenticated user and the session that identified them.
///
/// # Where this comes from
///
/// [`crate::auth::middleware::authenticate`] does the work — read the cookie,
/// look the session up, load the user, refresh the idle window — and puts the
/// result in the request extensions. This extractor only reads it back out.
///
/// The reason for the split is that the *forced-reset gate* has to see the user
/// too, and it is middleware. Loading the session twice per request (once for
/// the gate, once for the handler) would double the database work on every
/// authenticated call. So the middleware loads once and everything downstream
/// shares it.
///
/// # Failing closed
///
/// If the middleware did not run, the extension is absent and this returns 401
/// rather than silently letting the request through. A route that is accidentally
/// mounted outside the authenticated tree therefore rejects everyone, which is
/// the failure everybody notices immediately — as opposed to admitting everyone,
/// which is the failure nobody notices until it matters.
///
/// The `Arc`s make this cheap to clone out of the extensions.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    /// The authenticated user, freshly loaded for this request.
    ///
    /// Fresh, not cached: a role change or a deactivation takes effect on the
    /// user's very next request, with no session rotation needed.
    pub user: Arc<User>,
    /// The session that authenticated them.
    pub session: Arc<Session>,
}

impl CurrentUser {
    /// The user's id.
    pub fn id(&self) -> &str {
        &self.user.id
    }

    /// Whether the user holds at least `required`.
    pub fn has_role(&self, required: Role) -> bool {
        self.user.role.at_least(required)
    }

    /// Requires at least `required`.
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] (403) — never 401. The caller *is* authenticated;
    /// re-authenticating would not help, and telling a browser 401 invites it to
    /// throw a basic-auth dialog at a logged-in user.
    pub fn require_role(&self, required: Role) -> Result<(), AppError> {
        if self.has_role(required) {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or(AppError::Unauthorized)
    }
}

/// `Option<CurrentUser>`: authentication as a question rather than a demand.
///
/// axum 0.8 routes `Option<T>` through a separate trait rather than treating any
/// extractor's rejection as `None`, which is the right call — it means "optional"
/// is a decision the extractor makes, not an accident of a handler's signature.
///
/// Exactly one handler wants this: `POST /auth/logout`, where logging out when
/// already logged out has to be a success. Every other route wants the 401.
impl<S> OptionalFromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    // Reading an extension cannot fail; absence is the `None`, not an error.
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(parts.extensions.get::<Self>().cloned())
    }
}

/// A [`CurrentUser`] that is an admin, or a 403.
///
/// Taking this in a handler's signature *is* the authorisation check: a route
/// that names it cannot forget to call anything, and a route that does not name
/// it is visibly not admin-only.
#[derive(Debug, Clone)]
pub struct RequireAdmin(pub CurrentUser);

impl<S> FromRequestParts<S> for RequireAdmin
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let current =
            <CurrentUser as FromRequestParts<S>>::from_request_parts(parts, state).await?;
        current.require_role(Role::Admin)?;
        Ok(Self(current))
    }
}

/// A [`CurrentUser`] that can write, or a 403. Members and admins.
#[derive(Debug, Clone)]
pub struct RequireMember(pub CurrentUser);

impl<S> FromRequestParts<S> for RequireMember
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let current =
            <CurrentUser as FromRequestParts<S>>::from_request_parts(parts, state).await?;
        current.require_role(Role::Member)?;
        Ok(Self(current))
    }
}

/// Reads the client's address and user agent from a request.
///
/// # Which address
///
/// In order: `X-Forwarded-For`'s first hop, `X-Real-IP`, then the peer address
/// from [`ConnectInfo`].
///
/// Preferring the proxy headers is a deliberate trade, and it goes the opposite
/// way to the usual advice, so it is worth stating plainly. Trusting
/// `X-Forwarded-For` means an attacker can forge it and evade the *per-IP*
/// lockout. Not trusting it means that in the normal self-hosted deployment —
/// Atlas behind nginx or Caddy — every request arrives from `127.0.0.1`, so ten
/// failures from anyone lock the per-IP counter for *everybody*. The second is a
/// guaranteed denial of service in the default configuration; the first costs an
/// attacker nothing they did not already have, because the **per-username**
/// counter is what actually protects an account and no header can touch it.
///
/// The honest fix is a trusted-proxy setting, so `X-Forwarded-For` is believed
/// only from a configured hop. That is a config change, and config is Phase 20's
/// security pass (`TODO.md`) — this comment is the marker for it.
///
/// [`ConnectInfo`] is only present when the server was started with
/// `into_make_service_with_connect_info`. When it is absent and no proxy header
/// is set, the address is `None` and the per-IP counter is **skipped entirely**
/// rather than keyed on a placeholder — a shared `ip:unknown` counter would let
/// any ten failures anywhere lock out every login on the instance.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo(pub Client);

impl<S> FromRequestParts<S> for ClientInfo
where
    S: Send + Sync,
{
    // Infallible: an unknown client is a fact to record, not a request to reject.
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let ip = client_ip(parts);

        let user_agent = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            // A User-Agent is attacker-controlled and lands in a table the UI
            // renders. Bound it here; the frontend still has to escape it.
            .map(|value| truncate(value, MAX_USER_AGENT));

        Ok(Self(Client { ip, user_agent }))
    }
}

/// Longest `User-Agent` stored. Real ones are ~150 characters.
const MAX_USER_AGENT: usize = 512;

/// Longest address string stored. An IPv6 address with a zone is ~60.
const MAX_IP: usize = 64;

fn client_ip(parts: &Parts) -> Option<String> {
    let forwarded = parts
        .headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        // "client, proxy1, proxy2" — the first hop is the client.
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(ip) = forwarded {
        return Some(truncate(ip, MAX_IP));
    }

    let real_ip = parts
        .headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(ip) = real_ip {
        return Some(truncate(ip, MAX_IP));
    }

    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_string())
}

/// Truncates on a character boundary, so a multi-byte header cannot panic here.
fn truncate(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        None => value.to_owned(),
        Some((index, _)) => value[..index].to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    /// Builds request parts. Named `request_parts`, not `parts`, because the
    /// tests below bind their result to a local called `parts` — which would
    /// shadow the helper for the rest of the block.
    fn request_parts(
        build: impl FnOnce(axum::http::request::Builder) -> axum::http::request::Builder,
    ) -> Parts {
        build(Request::builder().uri("/"))
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    fn user(role: Role) -> User {
        User {
            id: "u1".to_owned(),
            username: "someone".to_owned(),
            email: None,
            display_name: "Someone".to_owned(),
            avatar_url: None,
            password_hash: "x".to_owned(),
            role,
            is_active: true,
            must_change_password: false,
            created_at: crate::auth::now(),
            updated_at: crate::auth::now(),
            last_login_at: None,
        }
    }

    fn session() -> Session {
        Session {
            id: "s1".to_owned(),
            user_id: "u1".to_owned(),
            created_at: crate::auth::now(),
            last_seen_at: crate::auth::now(),
            expires_at: crate::auth::now() + chrono::Duration::days(30),
            user_agent: None,
            ip: None,
        }
    }

    fn current(role: Role) -> CurrentUser {
        CurrentUser {
            user: Arc::new(user(role)),
            session: Arc::new(session()),
        }
    }

    #[tokio::test]
    async fn a_request_with_no_authentication_is_rejected_not_admitted() {
        // The fail-closed property: if the middleware never ran, nobody gets in.
        let mut parts = request_parts(|b| b);
        let err = <CurrentUser as FromRequestParts<()>>::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[tokio::test]
    async fn the_current_user_comes_back_out_of_the_extensions() {
        let mut parts = request_parts(|b| b);
        parts.extensions.insert(current(Role::Member));

        let extracted = <CurrentUser as FromRequestParts<()>>::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(extracted.id(), "u1");
        assert_eq!(extracted.user.role, Role::Member);
    }

    #[tokio::test]
    async fn require_admin_admits_admins_and_forbids_everyone_else() {
        for (role, allowed) in [
            (Role::Admin, true),
            (Role::Member, false),
            (Role::Viewer, false),
        ] {
            let mut parts = request_parts(|b| b);
            parts.extensions.insert(current(role));

            let result = RequireAdmin::from_request_parts(&mut parts, &()).await;
            assert_eq!(
                result.is_ok(),
                allowed,
                "{role} should be allowed: {allowed}"
            );

            if let Err(err) = result {
                // 403, not 401: they are authenticated, they are just not an
                // admin, and logging in again would not change that.
                assert!(
                    matches!(err, AppError::Forbidden),
                    "{role} got the wrong status"
                );
            }
        }
    }

    #[tokio::test]
    async fn require_member_admits_members_and_admins_but_not_viewers() {
        for (role, allowed) in [
            (Role::Admin, true),
            (Role::Member, true),
            (Role::Viewer, false),
        ] {
            let mut parts = request_parts(|b| b);
            parts.extensions.insert(current(role));
            assert_eq!(
                RequireMember::from_request_parts(&mut parts, &())
                    .await
                    .is_ok(),
                allowed,
                "{role}"
            );
        }
    }

    #[tokio::test]
    async fn a_role_guard_on_an_unauthenticated_request_is_a_401_not_a_403() {
        // "Log in" and "you may not" are different instructions to a client.
        let mut parts = request_parts(|b| b);
        let err = RequireAdmin::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized));
    }

    #[tokio::test]
    async fn the_client_address_prefers_the_first_forwarded_hop() {
        let mut parts = request_parts(|b| {
            b.header("x-forwarded-for", "203.0.113.7, 10.0.0.1, 10.0.0.2")
                .header("x-real-ip", "10.9.9.9")
        });
        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(client.ip, Some("203.0.113.7".to_owned()));
    }

    #[tokio::test]
    async fn the_client_address_falls_back_to_x_real_ip_then_to_nothing() {
        let mut parts = request_parts(|b| b.header("x-real-ip", "203.0.113.9"));
        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(client.ip, Some("203.0.113.9".to_owned()));

        // No headers and no ConnectInfo: None, deliberately. A placeholder here
        // would become one shared per-IP lockout counter for the whole instance.
        let mut parts = request_parts(|b| b);
        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(client.ip, None);
    }

    #[tokio::test]
    async fn the_peer_address_is_used_when_there_is_no_proxy() {
        let mut parts = request_parts(|b| b);
        parts.extensions.insert(ConnectInfo(
            "198.51.100.4:51234".parse::<SocketAddr>().unwrap(),
        ));

        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        // The address, without the ephemeral port — which would make every
        // request from one host a different lockout key.
        assert_eq!(client.ip, Some("198.51.100.4".to_owned()));
    }

    #[tokio::test]
    async fn an_empty_forwarded_header_does_not_shadow_the_peer_address() {
        let mut parts = request_parts(|b| b.header("x-forwarded-for", ""));
        parts.extensions.insert(ConnectInfo(
            "198.51.100.4:51234".parse::<SocketAddr>().unwrap(),
        ));

        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(client.ip, Some("198.51.100.4".to_owned()));
    }

    #[tokio::test]
    async fn an_over_long_user_agent_is_truncated_rather_than_stored_whole() {
        let huge = "a".repeat(MAX_USER_AGENT * 4);
        let mut parts = request_parts(|b| b.header(header::USER_AGENT, &huge));
        let ClientInfo(client) = ClientInfo::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(client.user_agent.unwrap().chars().count(), MAX_USER_AGENT);
    }

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // Slicing a multi-byte string at a byte index panics. The header is
        // attacker-controlled, so this must not be reachable.
        let multibyte = "日本語のユーザーエージェント".repeat(100);
        let cut = truncate(&multibyte, 5);
        assert_eq!(cut.chars().count(), 5);
        assert_eq!(cut, "日本語のユ");
    }
}
