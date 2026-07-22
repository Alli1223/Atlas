//! HTTP surface: router assembly, health check, and the OpenAPI document.

pub mod auth;
pub mod board;
pub mod cards;
pub mod comments;
pub mod members;
pub mod middleware;
pub mod project_config;
pub mod projects;
pub mod search;
pub mod serde_ext;
pub mod tags;
pub mod users;
pub mod workflow;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Json;
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

use crate::config::Config;
use crate::db::Db;
use crate::error::{AppResult, Problem};

/// Where the Swagger UI is served.
pub const DOCS_PATH: &str = "/api/docs";

/// Where the raw OpenAPI document is served.
///
/// Deliberately outside [`DOCS_PATH`] so the UI's `/{*rest}` route cannot
/// shadow it.
pub const OPENAPI_JSON_PATH: &str = "/api/openapi.json";

/// The versioned API prefix. Everything under it is free to change only across
/// a version bump.
pub const API_V1_PREFIX: &str = "/api/v1";

/// Shared state handed to every handler.
///
/// Cheap to clone: both fields are handles.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The database pools.
    pub db: Db,
    /// Resolved configuration.
    pub config: Arc<Config>,
}

impl AppState {
    /// Builds application state from an open database and its configuration.
    pub fn new(db: Db, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
        }
    }
}

/// The OpenAPI document. Paths are collected from the router, so the spec cannot
/// drift away from the routes that actually exist.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Atlas API",
        description = "Self-hosted project management with GitHub, Claude Code and Gemini integration.",
        license(name = "AGPL-3.0-or-later")
    ),
    tags(
        (name = "system", description = "Health and service metadata"),
        (name = "auth", description = "Sign-in, sign-out, password change, and sessions"),
        (name = "users", description = "User administration. Admin only"),
        (name = "projects", description = "Projects and their lifecycle"),
        (name = "project-members", description = "Per-project access: who may do what, and where"),
        (name = "project-config", description = "Per-project hierarchy, card types, statuses, priorities and resolutions"),
        (name = "cards", description = "Cards, the board, the hierarchy, and the changelog"),
        (name = "boards", description = "Board data (columns, mini-map rollups, swimlanes) and saved board config"),
        (name = "comments", description = "Comments on cards"),
        (name = "tags", description = "Free-text labels on cards, their presets, and merging"),
        (name = "workflows", description = "Workflows, transitions, their gates, and taking a transition"),
        (name = "search", description = "AQL search, query validation, and saved filters")
    )
)]
struct ApiDoc;

/// Health check response.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Always `ok`; a failing check is reported by the status code.
    #[schema(example = "ok")]
    pub status: String,
    /// The running Atlas version.
    #[schema(example = "0.1.0")]
    pub version: String,
    /// Always `ok`; the database was queried to produce this.
    #[schema(example = "ok")]
    pub db: String,
}

/// Liveness and readiness in one.
///
/// Actually queries the database rather than reporting a cached flag: a health
/// check that cannot fail is not a health check. Both pools are exercised,
/// because a reader that works while the writer is wedged is not "healthy".
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "system",
    responses(
        (status = 200, description = "Service and database are healthy", body = HealthResponse),
        (status = 500, description = "The database could not be reached", body = Problem),
    )
)]
async fn healthz(State(state): State<AppState>) -> AppResult<Json<HealthResponse>> {
    state.db.ping().await?;

    Ok(Json(HealthResponse {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        db: "ok".to_owned(),
    }))
}

/// Handles any route that does not exist, so a typo'd URL produces the same
/// problem+json shape as every other error rather than an empty 404.
async fn not_found() -> crate::error::AppError {
    crate::error::AppError::NotFound
}

/// The `/api/v1` surface.
///
/// # The three layers, and why they are here rather than on each route
///
/// All three wrap the *whole* `/api/v1` tree, which is what makes them impossible
/// for a new route to miss. `.layer` applies outside-in in reverse, so the order
/// below reads bottom-to-top: `verify_origin`, then `authenticate`, then
/// `authorise`.
///
/// - **`verify_origin`** is outermost, so a cross-site write is refused before
///   it can touch the database or spend any CPU.
/// - **`authenticate`** turns the session cookie into a
///   [`crate::auth::CurrentUser`] in the request extensions, and enforces the
///   forced-reset gate. It does not itself require a session — that decision
///   belongs in each handler's signature, which is why `POST /auth/login` can
///   live under the same layer.
/// - **`authorise`** is innermost, because it needs the `CurrentUser` the layer
///   above it produced. It decides which project the route is about and whether
///   the caller may reach it. **It refuses any route not classified in
///   [`crate::auth::project_access::SCOPES`]**, so a project-scoped route added
///   in Phase 8 that nobody classified returns 500 on its first request rather
///   than silently serving every project to everyone.
///
/// Anything mounted here is gated by default, three times over. That is the
/// property worth having: forgetting a gate on a new route should be impossible,
/// not merely discouraged.
fn api_v1(state: &AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(auth::routes())
        .merge(users::routes())
        .merge(projects::routes())
        .merge(members::routes())
        .merge(project_config::routes())
        .merge(cards::routes())
        .merge(board::routes())
        .merge(comments::routes())
        .merge(tags::routes())
        .merge(search::routes())
        .merge(workflow::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::project_access::authorise,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::authenticate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::verify_origin,
        ))
}

/// Assembles the complete application.
///
/// # Panics
///
/// If any route under [`API_V1_PREFIX`] has no entry in
/// [`crate::auth::project_access::SCOPES`], or vice versa — see
/// [`crate::auth::project_access::assert_scopes_match_routes`].
///
/// A panic here means the binary does not boot and every test that builds a
/// router fails on its first line, which is the point: a route whose access
/// rules nobody wrote down must be impossible to *ship*, not merely refused at
/// runtime. It is checked here because this is the first moment both halves
/// exist — the route set and the table — and it cannot be checked any later
/// without the answer being "some requests already happened".
pub fn router(state: AppState) -> Router {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(healthz))
        .nest(API_V1_PREFIX, api_v1(&state))
        .split_for_parts();

    // The OpenAPI document is generated from the routes themselves by
    // `utoipa_axum::routes!`, so it is the router's own account of its surface
    // rather than a second list that could drift from it.
    //
    // Two assertions, because there are two ways to ship an unguarded route:
    // mount it inside the nest and forget to classify it, or mount it outside the
    // nest, where there is nothing to forget because nothing applies.
    crate::auth::project_access::assert_scopes_match_routes(&openapi);
    crate::auth::project_access::assert_no_route_escapes_the_gate(&openapi);

    let router = router
        .merge(SwaggerUi::new(DOCS_PATH).url(OPENAPI_JSON_PATH, openapi))
        .fallback(not_found);

    middleware::apply(router, &state.config).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;
    use crate::test_support::TempDb;

    #[test]
    fn the_openapi_document_includes_healthz_and_uses_axum_08_syntax() {
        let (_router, openapi) = OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
            .routes(routes!(healthz))
            .split_for_parts();

        let json = openapi.to_json().unwrap();
        assert!(json.contains("/healthz"), "{json}");
        assert!(json.contains("Atlas API"), "{json}");
        // axum 0.8 uses `{id}`; a stray 0.7-style `:id` would panic at runtime,
        // so assert the spec never learns that syntax.
        assert!(
            !json.contains("/:"),
            "0.7-style route parameter in the spec"
        );
    }

    #[tokio::test]
    async fn the_router_builds_without_panicking() {
        // `Router::nest` and the route syntax are validated at construction, so
        // simply building the real router is a meaningful test.
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();
        let _router = router(AppState::new(db.clone(), temp.config()));
        db.close().await;
    }
}
