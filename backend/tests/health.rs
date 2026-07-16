//! End-to-end tests over the real router, real middleware and a real database.
//!
//! These drive the app through `tower::ServiceExt::oneshot` rather than binding
//! a port: no TCP, no races, and the full layer stack still runs. `axum-test`
//! would also work, but it is a dependency whose only job here would be to
//! rebuild `oneshot`.

use atlas::api::{self, AppState};
use atlas::db::{self, Db};
use atlas::test_support::TempDb;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

/// A migrated database and the router built over it.
///
/// `TempDb` is returned alongside because dropping it deletes the database.
async fn app() -> (Router, TempDb) {
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.expect("failed to open database");
    db::migrate::run(&db).await.expect("failed to migrate");
    (api::router(AppState::new(db, config)), temp)
}

async fn body_json(body: Body) -> Value {
    let bytes = to_bytes(body, 1024 * 1024)
        .await
        .expect("failed to read body");
    serde_json::from_slice(&bytes).expect("response body was not valid JSON")
}

#[tokio::test]
async fn healthz_returns_200_and_the_documented_body() {
    let (app, _temp) = app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );

    let json = body_json(response.into_body()).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["db"], "ok");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn healthz_carries_a_request_id() {
    let (app, _temp) = app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response.headers().contains_key("x-request-id"),
        "every response must be traceable back to a log line"
    );
}

#[tokio::test]
async fn healthz_fails_when_the_database_is_gone() {
    // The point of the check is that it can go red. If closing the pools still
    // yields 200, /healthz is decorative.
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.unwrap();
    db::migrate::run(&db).await.unwrap();

    let app = api::router(AppState::new(db.clone(), config));
    db.close().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_json(response.into_body()).await;
    assert_eq!(json["type"], "urn:atlas:error:internal");
    // Even a genuine database failure must not describe itself to the client.
    assert!(
        !json.to_string().to_lowercase().contains("pool"),
        "database internals leaked: {json}"
    );
}

#[tokio::test]
async fn an_unknown_route_returns_an_rfc_7807_document() {
    let (app, _temp) = app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );

    let json = body_json(response.into_body()).await;
    // The full RFC 7807 member set.
    assert_eq!(json["type"], "urn:atlas:error:not-found");
    assert_eq!(json["title"], "Not Found");
    assert_eq!(json["status"], 404);
    assert!(json["detail"].is_string(), "{json}");
    assert_eq!(json["instance"], "/api/v1/does-not-exist");
}

#[tokio::test]
async fn the_openapi_document_is_served() {
    let (app, _temp) = app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(api::OPENAPI_JSON_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response.into_body()).await;
    assert_eq!(json["info"]["title"], "Atlas API");
    assert!(json["paths"]["/healthz"].is_object(), "{json}");
}

#[tokio::test]
async fn the_swagger_ui_is_served() {
    let (app, _temp) = app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(api::DOCS_PATH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // The UI redirects to its trailing-slash form before rendering.
    assert!(
        response.status().is_success() || response.status().is_redirection(),
        "unexpected status for {}: {}",
        api::DOCS_PATH,
        response.status()
    );
}

#[tokio::test]
async fn migrations_ran_and_are_visible_through_the_app_database() {
    let temp = TempDb::new();
    let config = temp.config();
    let db = Db::connect(&config).await.unwrap();
    db::migrate::run(&db).await.unwrap();

    let version: String =
        sqlx::query_scalar("SELECT value FROM _atlas_meta WHERE key = 'schema_version'")
            .fetch_one(db.reader())
            .await
            .unwrap();
    assert_eq!(version, "1");

    db.close().await;
}
