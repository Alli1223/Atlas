//! End-to-end tests for Gemini generation, against a local stand-in for Google's API — no
//! network, no cost. The stand-in reproduces the shapes `docs/research/gemini-api.md`
//! verified: base64-inline images among text parts, a 400 `API_KEY_INVALID` for a bad key,
//! `promptFeedback.blockReason` for a refused prompt, and 429 for no image quota.

mod common;

use atlas::config::SecretString;
use atlas::secrets::vault::Validator;
use axum::Router;
use axum::extract::Json as AxumJson;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get as route_get, post as route_post};
use base64::Engine;
use common::*;
use serde_json::{Value, json};

const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

fn key_of(headers: &HeaderMap) -> String {
    headers
        .get("x-goog-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn invalid_key() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        AxumJson(json!({ "error": { "code": 400, "message": "API key not valid.", "status": "INVALID_ARGUMENT",
            "details": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "API_KEY_INVALID" }] } })),
    )
        .into_response()
}

async fn generate(headers: HeaderMap, AxumJson(body): AxumJson<Value>) -> axum::response::Response {
    match key_of(&headers).as_str() {
        "good-key" => {}
        "no-quota" => return (StatusCode::TOO_MANY_REQUESTS, AxumJson(json!({ "error": { "code": 429, "status": "RESOURCE_EXHAUSTED", "message": "quota" } }))).into_response(),
        _ => return invalid_key(),
    }
    let prompt = body["contents"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default();
    if prompt.contains("BLOCKME") {
        return AxumJson(json!({ "promptFeedback": { "blockReason": "SAFETY" } })).into_response();
    }
    if body["generationConfig"]["responseMimeType"] == "application/json" {
        return AxumJson(json!({
            "candidates": [{ "content": { "parts": [{ "text": "{\"aql\": \"status = Done AND updated <= -2w\"}" }] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 20 },
        }))
        .into_response();
    }
    if body["generationConfig"]["responseModalities"].is_array() {
        let png =
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake image bytes");
        return AxumJson(json!({
            "candidates": [{ "content": { "parts": [
                { "text": "Here is an image." },
                { "inlineData": { "mimeType": "image/png", "data": png } },
            ] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 30, "candidatesTokenCount": 1120 },
        }))
        .into_response();
    }
    AxumJson(json!({
        "candidates": [{ "content": { "parts": [{ "text": "## Goal\nShip it.\n\n- [ ] Tests pass" }] }, "finishReason": "STOP" }],
        "usageMetadata": { "promptTokenCount": 50, "candidatesTokenCount": 40 },
    }))
    .into_response()
}

async fn models(headers: HeaderMap) -> axum::response::Response {
    if key_of(&headers) == "good-key" {
        AxumJson(json!({ "models": [{ "name": "models/gemini-3.1-flash-image" }] })).into_response()
    } else {
        invalid_key()
    }
}

async fn fake_gemini() -> String {
    let router = Router::new()
        .route("/v1/models/{*model}", route_post(generate))
        .route("/v1beta/models", route_get(models));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

async fn gemini_app(key: &str) -> (App, String) {
    let base = fake_gemini().await;
    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    config.master_key = Some(SecretString::new(MASTER_KEY));
    config.gemini_base_url = Some(base);
    let app = App::with_config(temp, config).await;
    let admin = admin_past_the_gate(&app).await;
    app.expect(
        post(
            "/api/v1/credentials",
            Some(&admin),
            json!({ "provider": "gemini", "label": "main", "secret": key }),
        ),
        StatusCode::CREATED,
    )
    .await;
    (app, admin)
}

#[tokio::test]
async fn options_are_generated_stored_costed_and_usable_as_the_avatar() {
    let (app, admin) = gemini_app("good-key").await;
    project(&app, &admin, "ART", "3d-modeling").await;

    let generation = app
        .expect(
            post(
                "/api/v1/projects/ART/generate/images",
                Some(&admin),
                json!({ "purpose": "projectAvatar", "style": "3d", "count": 2 }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(generation["status"], "succeeded");
    let images = generation["images"].as_array().expect("images").clone();
    assert_eq!(images.len(), 2, "two options, two requests");
    assert!(
        (generation["costUsd"].as_f64().unwrap_or(0.0) - 2.0 * 0.067).abs() < 1e-9,
        "{generation}"
    );
    let prompt = text(&generation, "prompt");
    assert!(
        prompt.contains("3D modelling") && prompt.contains("No text"),
        "{prompt}"
    );

    let url = text(&images[0], "url");
    let got = app.expect(get(&url, Some(&admin)), StatusCode::OK).await;
    assert!(got.bytes.starts_with(b"\x89PNG"));

    let id = text(&images[0], "id");
    app.expect(
        post(
            "/api/v1/projects/ART/avatar",
            Some(&admin),
            json!({ "imageId": id }),
        ),
        StatusCode::OK,
    )
    .await;
    let projects = app
        .expect(get("/api/v1/projects/ART", Some(&admin)), StatusCode::OK)
        .await
        .json();
    assert_eq!(projects["avatarUrl"], url.as_str());

    // Another project's image cannot be borrowed.
    project(&app, &admin, "OTHER", "blank").await;
    app.expect(
        post(
            "/api/v1/projects/OTHER/cover",
            Some(&admin),
            json!({ "imageId": id }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // The spend shows up on the cost dashboard.
    let costs = app
        .expect(
            get("/api/v1/reports/agent-costs", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    let gemini = costs["byProvider"]
        .as_array()
        .and_then(|p| p.iter().find(|r| r["key"] == "gemini"))
        .cloned()
        .expect("gemini spend");
    assert!(gemini["costUsd"].as_f64().unwrap_or(0.0) > 0.1);
}

#[tokio::test]
async fn a_reference_image_lands_on_the_card_as_an_attachment_and_can_be_its_cover() {
    let (app, admin) = gemini_app("good-key").await;
    let p = project(&app, &admin, "REF", "3d-modeling").await;
    let key = card(&app, &admin, &p, "Low-poly lighthouse").await;

    let generation = app
        .expect(
            post(
                "/api/v1/projects/REF/generate/images",
                Some(&admin),
                json!({ "purpose": "reference", "cardKey": key, "style": "photo" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert!(text(&generation, "prompt").contains("Low-poly lighthouse"));
    let attachments = app
        .expect(
            get(&format!("/api/v1/cards/{key}/attachments"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(attachments.len(), 1);
    assert!(text(&attachments[0], "filename").starts_with("reference-"));

    let image = text(&generation["images"][0], "id");
    app.expect(
        post(
            &format!("/api/v1/cards/{key}/cover"),
            Some(&admin),
            json!({ "imageId": image }),
        ),
        StatusCode::OK,
    )
    .await;
    let cover = app
        .expect(
            get(&format!("/api/v1/cards/{key}/cover"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert!(cover["url"].as_str().unwrap_or_default().ends_with(&image));
}

#[tokio::test]
async fn a_blocked_prompt_is_recorded_and_explained_not_crashed_on() {
    let (app, admin) = gemini_app("good-key").await;
    project(&app, &admin, "BLK", "blank").await;
    let reply = app
        .send(post(
            "/api/v1/projects/BLK/generate/images",
            Some(&admin),
            json!({ "purpose": "projectCover", "prompt": "BLOCKME please" }),
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw_body
    );
    assert!(
        reply.raw_body.contains("safety filter"),
        "{}",
        reply.raw_body
    );
    let history = app
        .expect(
            get("/api/v1/projects/BLK/generations", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(history[0]["status"], "blocked");
}

#[tokio::test]
async fn bad_keys_and_no_quota_are_told_apart() {
    let (app, admin) = gemini_app("wrong-key").await;
    project(&app, &admin, "KEY", "blank").await;
    let reply = app
        .send(post(
            "/api/v1/projects/KEY/generate/images",
            Some(&admin),
            json!({ "purpose": "projectCover" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
    assert!(
        reply.raw_body.contains("key was rejected"),
        "{}",
        reply.raw_body
    );

    let (app, admin) = gemini_app("no-quota").await;
    project(&app, &admin, "QTA", "blank").await;
    let reply = app
        .send(post(
            "/api/v1/projects/QTA/generate/images",
            Some(&admin),
            json!({ "purpose": "projectCover" }),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw_body);
    assert!(
        reply.raw_body.contains("no free tier"),
        "{}",
        reply.raw_body
    );
}

#[tokio::test]
async fn drafts_and_natural_language_queries() {
    let (app, admin) = gemini_app("good-key").await;
    let p = project(&app, &admin, "TXT", "programming").await;
    let key = card(&app, &admin, &p, "Add login").await;

    let draft = app
        .expect(
            post(
                "/api/v1/generate/text",
                Some(&admin),
                json!({ "purpose": "draft", "input": "email + password", "cardKey": key }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert!(text(&draft, "text").contains("- [ ]"));

    let query = app
        .expect(
            post(
                "/api/v1/generate/text",
                Some(&admin),
                json!({ "purpose": "query", "input": "done things nobody touched in two weeks" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(query["text"], "status = Done AND updated <= -2w");
    assert_eq!(query["valid"], true);
}

#[tokio::test]
async fn the_key_probe_reports_valid_and_invalid_keys() {
    let base = fake_gemini().await;
    let validator = atlas::integrations::gemini::GeminiValidator::with_base_url(base);
    let good = validator
        .validate(&atlas::secrets::Secret::new("good-key".to_owned()))
        .await
        .expect("probe");
    assert_eq!(good.status, atlas::secrets::CredentialStatus::Valid);
    let bad = validator
        .validate(&atlas::secrets::Secret::new("nope".to_owned()))
        .await
        .expect("probe");
    assert_eq!(
        bad.status,
        atlas::secrets::CredentialStatus::Invalid,
        "a 400 API_KEY_INVALID means invalid, not malformed"
    );
}
