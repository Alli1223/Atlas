//! Phase 20 operations and security, end to end: response security headers, backups and
//! offline restore, the credential probe rate limit, and repo link health against a local
//! stand-in for GitHub.

mod common;

use atlas::config::SecretString;
use atlas::integrations::github::client::GithubClient;
use atlas::integrations::github::health;
use atlas::integrations::github::store::{self, NewProjectRepo};
use axum::Router;
use axum::extract::{Json as AxumJson, Path};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get as route_get;
use common::*;
use serde_json::json;

const MASTER_KEY: &str = "dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=";

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let app = App::new().await;
    let reply = app.send(get("/healthz", None)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(reply.header(header::X_FRAME_OPTIONS), "DENY");
    let csp = reply.header(header::CONTENT_SECURITY_POLICY);
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(
        reply
            .headers
            .get(header::STRICT_TRANSPORT_SECURITY)
            .is_none(),
        "no HSTS without a known HTTPS origin"
    );

    // Errors carry them too.
    let missing = app.send(get("/api/v1/nope", None)).await;
    assert_eq!(missing.header(header::X_FRAME_OPTIONS), "DENY");

    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    config.public_url = Some("https://atlas.example.com".to_owned());
    let secure = App::with_config(temp, config).await;
    let reply = secure.send(get("/healthz", None)).await;
    assert!(
        reply
            .header(header::STRICT_TRANSPORT_SECURITY)
            .starts_with("max-age="),
        "HSTS once Atlas knows it is served over HTTPS"
    );
}

#[tokio::test]
async fn the_spa_inline_script_is_allowed_by_hash() {
    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    let static_dir = config.data_dir.join("static");
    std::fs::create_dir_all(&static_dir).expect("static dir");
    std::fs::write(
        static_dir.join("index.html"),
        "<html><head><script>window.theme = 1</script></head><body></body></html>",
    )
    .expect("index");
    config.static_dir = Some(static_dir);
    let app = App::with_config(temp, config).await;
    let csp = app
        .send(get("/", None))
        .await
        .header(header::CONTENT_SECURITY_POLICY);
    assert!(csp.contains("'sha256-"), "{csp}");
}

#[tokio::test]
async fn an_admin_backup_can_be_listed_downloaded_and_restored_offline() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "BAK", "blank").await;
    card(&app, &admin, &p, "Before the backup").await;

    let info = app
        .expect(
            post_empty("/api/v1/admin/backups", Some(&admin)),
            StatusCode::CREATED,
        )
        .await
        .json();
    let name = text(&info, "name");
    assert!(name.starts_with("atlas-"));
    assert!(info["databaseBytes"].as_u64().unwrap_or(0) > 0);
    assert!(info["uploadsBytes"].as_u64().is_some());

    let listed = app
        .expect(get("/api/v1/admin/backups", Some(&admin)), StatusCode::OK)
        .await
        .rows();
    assert_eq!(listed.len(), 1);

    let file = app
        .expect(
            get(
                &format!("/api/v1/admin/backups/{name}/database"),
                Some(&admin),
            ),
            StatusCode::OK,
        )
        .await;
    assert!(file.bytes.starts_with(b"SQLite format 3"));
    app.expect(
        get("/api/v1/admin/backups/..%2F..%2Fetc/database", Some(&admin)),
        StatusCode::NOT_FOUND,
    )
    .await;

    let (_, member) = user(&app, &admin, "mona", "member").await;
    app.expect(
        post_empty("/api/v1/admin/backups", Some(&member)),
        StatusCode::FORBIDDEN,
    )
    .await;

    // Work done after the backup is lost by restoring it — which is the point.
    card(&app, &admin, &p, "After the backup").await;
    let count = |db: atlas::db::Db| async move {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cards")
            .fetch_one(db.reader())
            .await
            .expect("count");
        n
    };
    assert_eq!(count(app.db.clone()).await, 2);

    let refused = atlas::backup::restore(&app.config, &name).await;
    assert!(refused.is_err(), "a live database is not restored over");

    app.db.close().await;
    atlas::backup::restore(&app.config, &name)
        .await
        .expect("restore");
    let reopened = atlas::db::Db::connect(&app.config).await.expect("reopen");
    assert_eq!(count(reopened.clone()).await, 1);
    reopened.close().await;
}

#[tokio::test]
async fn a_credential_probe_cannot_be_hammered() {
    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    config.master_key = Some(SecretString::new(MASTER_KEY));
    let app = App::with_config(temp, config).await;
    let admin = admin_past_the_gate(&app).await;
    let id = app
        .expect(
            post(
                "/api/v1/credentials",
                Some(&admin),
                json!({ "provider": "anthropic", "label": "main", "secret": "sk-ant-test-0000" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    let validate = || {
        post(
            &format!("/api/v1/credentials/{id}/validate"),
            Some(&admin),
            json!({}),
        )
    };
    app.expect(validate(), StatusCode::OK).await;
    let again = app.send(validate()).await;
    assert_eq!(
        again.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        again.raw_body
    );
    assert!(again.raw_body.contains("try again"), "{}", again.raw_body);
}

/// A stand-in for `GET /repos/{owner}/{repo}`: `acme/widget` is repo 7; `acme/old-name`
/// redirects (as GitHub does) to the renamed `acme/new-name`, still repo 7; a `bad-token`
/// is rejected; anything else is 404.
async fn fake_repo(
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> axum::response::Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if token.ends_with("bad-token") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let full_name = match (owner.as_str(), repo.as_str()) {
        ("acme", "widget") => "acme/widget",
        ("acme", "old-name") => "acme/new-name",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    AxumJson(json!({
        "id": 7, "full_name": full_name, "default_branch": "main", "private": true,
        "permissions": { "push": true },
    }))
    .into_response()
}

async fn fake_github() -> String {
    let router = Router::new().route("/repos/{owner}/{repo}", route_get(fake_repo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

async fn link(app: &App, project_id: &str, repo: &str) -> store::ProjectRepo {
    let mut tx = app.db.begin_write().await.expect("tx");
    let row = store::upsert_project_repo(
        &mut tx,
        &NewProjectRepo {
            project_id,
            credential_id: None,
            owner: "acme",
            repo,
            repo_id: 7,
            default_branch: "main",
            branch_prefix: "feature",
        },
        atlas::test_support::now(),
    )
    .await
    .expect("link");
    tx.commit().await.expect("commit");
    row
}

#[tokio::test]
async fn a_broken_repo_link_is_recorded_and_a_rename_is_followed() {
    let base = fake_github().await;
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "GIT", "programming").await;

    let client = |token: &str| {
        GithubClient::with_base_url(atlas::secrets::Secret::new(token.to_owned()), base.clone())
            .expect("client")
    };
    let now = atlas::test_support::now();

    let repo = link(&app, &p.id, "widget").await;
    let good = client("good-token");
    assert_eq!(
        health::check(&app.db, Some(&good), &repo, now)
            .await
            .unwrap(),
        "ok"
    );

    let bad = client("bad-token");
    assert_eq!(
        health::check(&app.db, Some(&bad), &repo, now)
            .await
            .unwrap(),
        "auth_failed"
    );
    let shown = app
        .expect(
            get("/api/v1/projects/GIT/repo", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(shown["syncStatus"], "auth_failed");
    assert!(text(&shown, "syncDetail").contains("rejected the token"));

    assert_eq!(
        health::check(&app.db, None, &repo, now).await.unwrap(),
        "no_credential"
    );

    let renamed = link(&app, &p.id, "old-name").await;
    assert_eq!(renamed.sync_status, "ok", "relinking resets the verdict");
    assert_eq!(
        health::check(&app.db, Some(&good), &renamed, now)
            .await
            .unwrap(),
        "ok"
    );
    let shown = app
        .expect(
            get("/api/v1/projects/GIT/repo", Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(
        shown["fullName"], "acme/new-name",
        "the rename was followed"
    );

    let gone = link(&app, &p.id, "deleted").await;
    assert_eq!(
        health::check(&app.db, Some(&good), &gone, now)
            .await
            .unwrap(),
        "not_found"
    );
}
