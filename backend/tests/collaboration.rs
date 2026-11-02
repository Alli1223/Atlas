//! End-to-end tests for the card's collaboration surface: watchers, worklogs, card links,
//! remote links, components, milestones, attachments and avatars.
//!
//! What each test pins is a claim a unit test cannot make — that the rows land, that the
//! inverse link exists, that a link never leaks a card the caller cannot see, that a version
//! is a version and not a second file, and that a downloaded `.html` is never served as a
//! page.

mod common;

use axum::http::{Method, StatusCode, header};
use common::*;
use serde_json::json;

#[tokio::test]
async fn watching_is_self_service_and_removing_someone_else_needs_admin() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "WAT", "blank").await;
    let key = card(&app, &admin, &p, "Watch me").await;
    let (viewer_id, viewer) = user(&app, &admin, "vera", "member").await;
    grant(&app, &admin, "WAT", &viewer_id, "viewer").await;

    // A project viewer can watch.
    let list = app
        .expect(
            post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&viewer)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["username"], "vera");

    // Idempotent.
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&viewer)),
        StatusCode::OK,
    )
    .await;

    // The admin can unwatch Vera; Vera cannot unwatch the admin.
    let admin_id = app
        .expect(get("/api/v1/auth/me", Some(&admin)), StatusCode::OK)
        .await
        .id();
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&admin)),
        StatusCode::OK,
    )
    .await;
    app.expect(
        delete(
            &format!("/api/v1/cards/{key}/watchers/{admin_id}"),
            Some(&viewer),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    app.expect(
        delete(
            &format!("/api/v1/cards/{key}/watchers/{viewer_id}"),
            Some(&admin),
        ),
        StatusCode::NO_CONTENT,
    )
    .await;
    let list = app
        .expect(
            get(&format!("/api/v1/cards/{key}/watchers"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert!(list.iter().all(|w| w["username"] != "vera"), "{list:?}");
}

#[tokio::test]
async fn worklogs_parse_the_duration_grammar_and_total() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "TIME", "programming").await;
    let key = card(&app, &admin, &p, "Timed").await;

    let first = app
        .expect(
            post(
                &format!("/api/v1/cards/{key}/worklogs"),
                Some(&admin),
                json!({ "duration": "1d 2h 30m", "note": "pairing" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(first["minutes"], 8 * 60 + 150);
    assert_eq!(first["source"], "manual");

    app.expect(
        post(
            &format!("/api/v1/cards/{key}/worklogs"),
            Some(&admin),
            json!({ "duration": "45" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    app.expect(
        post(
            &format!("/api/v1/cards/{key}/worklogs"),
            Some(&admin),
            json!({ "duration": "soon" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let summary = app
        .expect(
            get(&format!("/api/v1/cards/{key}/worklogs"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(summary["totalMinutes"], 8 * 60 + 150 + 45);
    assert_eq!(summary["totalDisplay"], "1d 3h 15m");

    // Edit and delete, as the author.
    let id = text(&first, "id");
    let edited = app
        .expect(
            patch(
                &format!("/api/v1/worklogs/{id}"),
                Some(&admin),
                json!({ "duration": "1h", "note": null }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(edited["minutes"], 60);
    assert!(edited["note"].is_null());
    assert!(edited["updatedAt"].is_string());

    // Someone else's worklog is not yours to edit.
    let (other_id, other) = user(&app, &admin, "otto", "member").await;
    grant(&app, &admin, "TIME", &other_id, "member").await;
    app.expect(
        patch(
            &format!("/api/v1/worklogs/{id}"),
            Some(&other),
            json!({ "duration": "8h" }),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    app.expect(
        delete(&format!("/api/v1/worklogs/{id}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
}

#[tokio::test]
async fn a_card_link_materialises_its_inverse_and_unlinks_as_a_pair() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "LNK", "programming").await;
    let a = card(&app, &admin, &p, "Blocker").await;
    let b = card(&app, &admin, &p, "Blocked").await;

    let links = app
        .expect(
            post(
                &format!("/api/v1/cards/{a}/links"),
                Some(&admin),
                json!({ "targetKey": b, "linkType": "blocks" }),
            ),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["linkType"], "blocks");
    assert_eq!(links[0]["cardKey"], b.as_str());

    let back = app
        .expect(
            get(&format!("/api/v1/cards/{b}/links"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0]["linkType"], "is_blocked_by");
    assert_eq!(back[0]["cardKey"], a.as_str());

    // AQL's linkedCards() sees it.
    let found = app
        .expect(
            post(
                "/api/v1/search",
                Some(&admin),
                json!({ "aql": format!("key IN linkedCards(\"{a}\")") }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(found["total"], 1, "{found}");

    // Self links are refused.
    app.expect(
        post(
            &format!("/api/v1/cards/{a}/links"),
            Some(&admin),
            json!({ "targetKey": a, "linkType": "relates_to" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // Removing from the inverse side removes both rows.
    let back_id = text(&back[0], "id");
    app.expect(
        delete(&format!("/api/v1/cards/{b}/links/{back_id}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
    for key in [&a, &b] {
        let rows = app
            .expect(
                get(&format!("/api/v1/cards/{key}/links"), Some(&admin)),
                StatusCode::OK,
            )
            .await
            .rows();
        assert_eq!(rows.len(), 0, "{key}: {rows:?}");
    }
}

#[tokio::test]
async fn a_link_never_reveals_or_reaches_a_card_the_caller_cannot_see() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let open = project(&app, &admin, "OPEN", "blank").await;
    let secret = project(&app, &admin, "SECRET", "blank").await;
    let mine = card(&app, &admin, &open, "Mine").await;
    let hidden = card(&app, &admin, &secret, "Classified summary").await;

    let (member_id, member) = user(&app, &admin, "mia", "member").await;
    grant(&app, &admin, "OPEN", &member_id, "member").await;

    // Mia cannot link to a card in a project she cannot see — and it 404s, not 403s.
    app.expect(
        post(
            &format!("/api/v1/cards/{mine}/links"),
            Some(&member),
            json!({ "targetKey": hidden, "linkType": "relates_to" }),
        ),
        StatusCode::NOT_FOUND,
    )
    .await;

    // The admin links them; Mia's view of her own card must not show the secret one.
    app.expect(
        post(
            &format!("/api/v1/cards/{mine}/links"),
            Some(&admin),
            json!({ "targetKey": hidden, "linkType": "relates_to" }),
        ),
        StatusCode::OK,
    )
    .await;
    let reply = app
        .expect(
            get(&format!("/api/v1/cards/{mine}/links"), Some(&member)),
            StatusCode::OK,
        )
        .await;
    assert_eq!(reply.rows().len(), 0);
    assert!(!reply.raw_body.contains("Classified"));
}

#[tokio::test]
async fn remote_links_accept_only_web_urls() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "WEB", "job-search").await;
    let key = card(&app, &admin, &p, "Acme application").await;

    let added = app
        .expect(
            post(
                &format!("/api/v1/cards/{key}/remote-links"),
                Some(&admin),
                json!({ "url": "https://jobs.example.com/123", "title": "Posting" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(added["title"], "Posting");

    for bad in [
        "javascript:alert(1)",
        "data:text/html,x",
        "ftp://x.y",
        "nope",
    ] {
        app.expect(
            post(
                &format!("/api/v1/cards/{key}/remote-links"),
                Some(&admin),
                json!({ "url": bad }),
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }

    let id = text(&added, "id");
    app.expect(
        delete(
            &format!("/api/v1/cards/{key}/remote-links/{id}"),
            Some(&admin),
        ),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(
        delete(
            &format!("/api/v1/cards/{key}/remote-links/{id}"),
            Some(&admin),
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn components_are_unique_per_project_and_scoped_to_it() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "CMP", "3d-modeling").await;
    let other = project(&app, &admin, "OTH", "3d-modeling").await;
    let key = card(&app, &admin, &p, "Hero asset").await;

    let rigging = app
        .expect(
            post(
                "/api/v1/projects/CMP/components",
                Some(&admin),
                json!({ "name": "Rigging" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    // Case-insensitively unique.
    app.expect(
        post(
            "/api/v1/projects/CMP/components",
            Some(&admin),
            json!({ "name": "rigging" }),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let foreign = app
        .expect(
            post(
                &format!("/api/v1/projects/{}/components", other.key),
                Some(&admin),
                json!({ "name": "Lighting" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();

    let set = app
        .expect(
            put(
                &format!("/api/v1/cards/{key}/components"),
                Some(&admin),
                json!({ "componentIds": [rigging] }),
            ),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["cardCount"], 1);

    app.expect(
        put(
            &format!("/api/v1/cards/{key}/components"),
            Some(&admin),
            json!({ "componentIds": [foreign] }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    app.expect(
        patch(
            &format!("/api/v1/components/{rigging}"),
            Some(&admin),
            json!({ "name": "Rig & Skin", "description": "Skeletons" }),
        ),
        StatusCode::OK,
    )
    .await;
    app.expect(
        delete(&format!("/api/v1/components/{rigging}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
    let left = app
        .expect(
            get(&format!("/api/v1/cards/{key}/components"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(left.len(), 0);
}

#[tokio::test]
async fn a_milestone_is_a_card_field_with_history_and_progress() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "REL", "programming").await;
    let key = card(&app, &admin, &p, "Ship it").await;

    let v1 = app
        .expect(
            post(
                "/api/v1/projects/REL/milestones",
                Some(&admin),
                json!({ "name": "v1.0", "startDate": "2026-01-01", "releaseDate": "2026-02-01" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    app.expect(
        post(
            "/api/v1/projects/REL/milestones",
            Some(&admin),
            json!({ "name": "backwards", "startDate": "2026-02-01", "releaseDate": "2026-01-01" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let updated = app
        .expect(
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "milestoneId": v1 }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert_eq!(updated["milestoneId"], v1.as_str());

    let history = app
        .expect(
            get(&format!("/api/v1/cards/{key}/history"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    let change = history
        .iter()
        .find(|h| h["field"] == "milestone")
        .unwrap_or_else(|| panic!("no milestone history in {history:?}"));
    assert_eq!(change["toDisplay"], "v1.0");

    // Another project's milestone is refused.
    let other = project(&app, &admin, "OTR", "programming").await;
    let theirs = app
        .expect(
            post(
                &format!("/api/v1/projects/{}/milestones", other.key),
                Some(&admin),
                json!({ "name": "theirs" }),
            ),
            StatusCode::CREATED,
        )
        .await
        .id();
    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "milestoneId": theirs }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let released = app
        .expect(
            patch(
                &format!("/api/v1/milestones/{v1}"),
                Some(&admin),
                json!({ "status": "released" }),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    assert!(released["releasedAt"].is_string());
    assert_eq!(released["cardCount"], 1);

    // Deleting the milestone un-schedules the card rather than deleting it.
    app.expect(
        delete(&format!("/api/v1/milestones/{v1}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert!(read_card(&app, &admin, &key).await["milestoneId"].is_null());
}

#[tokio::test]
async fn re_uploading_a_name_adds_a_version_and_every_version_downloads() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "ATT", "3d-modeling").await;
    let key = card(&app, &admin, &p, "Hero asset").await;

    let first = app
        .expect(
            raw(
                Method::POST,
                &format!("/api/v1/cards/{key}/attachments?filename=model.blend"),
                Some(&admin),
                "application/octet-stream",
                b"version one".to_vec(),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(first["currentVersion"], 1);
    let id = text(&first, "id");

    let second = app
        .expect(
            raw(
                Method::POST,
                &format!("/api/v1/cards/{key}/attachments?filename=model.blend"),
                Some(&admin),
                "application/octet-stream",
                b"version two, longer".to_vec(),
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    assert_eq!(second["id"], id.as_str(), "same name, same attachment");
    assert_eq!(second["currentVersion"], 2);
    assert_eq!(second["sizeBytes"], 19);

    let list = app
        .expect(
            get(&format!("/api/v1/cards/{key}/attachments"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(list.len(), 1);

    let versions = app
        .expect(
            get(&format!("/api/v1/attachments/{id}/versions"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(versions.len(), 2);
    assert_ne!(versions[0]["sha256"], versions[1]["sha256"]);

    let newest = app
        .expect(
            get(&format!("/api/v1/attachments/{id}/content"), Some(&admin)),
            StatusCode::OK,
        )
        .await;
    assert_eq!(newest.bytes, b"version two, longer");
    let oldest = app
        .expect(
            get(
                &format!("/api/v1/attachments/{id}/content?version=1"),
                Some(&admin),
            ),
            StatusCode::OK,
        )
        .await;
    assert_eq!(oldest.bytes, b"version one");

    app.expect(
        delete(&format!("/api/v1/attachments/{id}"), Some(&admin)),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(
        get(&format!("/api/v1/attachments/{id}/content"), Some(&admin)),
        StatusCode::NOT_FOUND,
    )
    .await;
    let left = std::fs::read_dir(app.config.data_dir.join("uploads/attachments"))
        .map_or(0, Iterator::count);
    assert_eq!(left, 0, "deleting the attachment removes its files");
}

#[tokio::test]
async fn an_uploaded_page_downloads_as_bytes_and_never_renders_on_our_origin() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "XSS", "blank").await;
    let key = card(&app, &admin, &p, "Payloads").await;

    for (name, kind) in [
        ("../../evil.html", "text/html"),
        ("pic.svg", "image/svg+xml"),
    ] {
        let up = app
            .expect(
                raw(
                    Method::POST,
                    &format!(
                        "/api/v1/cards/{key}/attachments?filename={}",
                        name.replace('/', "%2F")
                    ),
                    Some(&admin),
                    kind,
                    b"<script>alert(document.cookie)</script>".to_vec(),
                ),
                StatusCode::CREATED,
            )
            .await
            .json();
        assert!(!text(&up, "filename").contains('/'), "{up}");
        let id = text(&up, "id");
        let got = app
            .expect(
                get(&format!("/api/v1/attachments/{id}/content"), Some(&admin)),
                StatusCode::OK,
            )
            .await;
        assert_eq!(got.header(header::CONTENT_TYPE), "application/octet-stream");
        assert!(
            got.header(header::CONTENT_DISPOSITION)
                .starts_with("attachment")
        );
        assert_eq!(got.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        assert!(
            got.header(header::CONTENT_SECURITY_POLICY)
                .contains("sandbox")
        );
    }

    // A PNG is previewable inline.
    let png = b"\x89PNG\r\n\x1a\n0000".to_vec();
    let up = app
        .expect(
            raw(
                Method::POST,
                &format!("/api/v1/cards/{key}/attachments?filename=shot.png"),
                Some(&admin),
                "image/png",
                png,
            ),
            StatusCode::CREATED,
        )
        .await
        .json();
    let id = text(&up, "id");
    let got = app
        .expect(
            get(&format!("/api/v1/attachments/{id}/content"), Some(&admin)),
            StatusCode::OK,
        )
        .await;
    assert_eq!(got.header(header::CONTENT_TYPE), "image/png");
    assert!(
        got.header(header::CONTENT_DISPOSITION)
            .starts_with("inline")
    );
}

#[tokio::test]
async fn an_upload_over_the_limit_is_refused_and_leaves_nothing() {
    let temp = atlas::test_support::TempDb::new();
    let mut config = temp.config();
    config.max_upload_mb = 1;
    let app = App::with_config(temp, config).await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "BIG", "blank").await;
    let key = card(&app, &admin, &p, "Huge").await;

    let reply = app
        .send(raw(
            Method::POST,
            &format!("/api/v1/cards/{key}/attachments?filename=big.bin"),
            Some(&admin),
            "application/octet-stream",
            vec![0u8; 1024 * 1024 + 1],
        ))
        .await;
    assert!(
        reply.status == StatusCode::UNPROCESSABLE_ENTITY
            || reply.status == StatusCode::PAYLOAD_TOO_LARGE,
        "{} {}",
        reply.status,
        reply.raw_body
    );
    let rows = app
        .expect(
            get(&format!("/api/v1/cards/{key}/attachments"), Some(&admin)),
            StatusCode::OK,
        )
        .await
        .rows();
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
async fn an_avatar_must_really_be_an_image() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;

    app.expect(
        raw(
            Method::PUT,
            "/api/v1/auth/me/avatar",
            Some(&admin),
            "image/png",
            b"<svg onload=alert(1)>".to_vec(),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let me = app
        .expect(
            raw(
                Method::PUT,
                "/api/v1/auth/me/avatar",
                Some(&admin),
                "application/octet-stream",
                b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
            ),
            StatusCode::OK,
        )
        .await
        .json();
    let url = text(&me, "avatarUrl");
    assert!(url.starts_with("/api/v1/users/"), "{url}");
    let got = app.expect(get(&url, Some(&admin)), StatusCode::OK).await;
    assert_eq!(got.header(header::CONTENT_TYPE), "image/png");

    app.expect(
        request(Method::DELETE, "/api/v1/auth/me/avatar", Some(&admin), None),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(get(&url, Some(&admin)), StatusCode::NOT_FOUND)
        .await;
}
