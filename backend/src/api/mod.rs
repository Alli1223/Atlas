//! HTTP surface: router assembly, health check, and the OpenAPI document.

pub mod middleware;

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
        (name = "system", description = "Health and service metadata")
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

/// The `/api/v1` surface. Populated from Phase 2 onwards.
fn api_v1() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}

/// Assembles the complete application.
pub fn router(state: AppState) -> Router {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(healthz))
        .nest(API_V1_PREFIX, api_v1())
        .split_for_parts();

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
