//! End-to-end tests for the activity feed and notifications: who hears about what, the
//! "don't notify me of my own changes" default, access, coalescing, mentions, preferences,
//! and the feed seeing writes from outside the request path.

mod common;

use atlas::jobs;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn inbox(app: &App, cookie: &str) -> Value {
    app.expect(get("/api/v1/notifications", Some(cookie)), StatusCode::OK)
        .await
        .json()
}

async fn drain(app: &App) -> usize {
    jobs::pump_until_idle(&app.state())
        .await
        .expect("pump failed")
}

#[tokio::test]
async fn assigning_notifies_the_assignee_but_never_yourself() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "N", "programming").await;
    let (alice_id, alice) = user(&app, &admin, "alice", "member").await;
    grant(&app, &admin, "N", &alice_id, "member").await;
    let key = card(&app, &admin, &p, "Do the thing").await;
    drain(&app).await;

    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": alice_id }),
        ),
        StatusCode::OK,
    )
    .await;
    assert!(drain(&app).await >= 1);

    let bell = inbox(&app, &alice).await;
    assert_eq!(bell["unread"], 1, "{bell}");
    assert_eq!(bell["items"][0]["kind"], "assigned");
    assert_eq!(bell["items"][0]["cardKey"], key.as_str());
    assert_eq!(bell["items"][0]["link"], format!("/cards/{key}"));

    // Alice assigning the card to herself tells nobody — least of all Alice.
    let key2 = card(&app, &admin, &p, "Another").await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key2}"),
            Some(&alice),
            json!({ "assigneeId": alice_id }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    let bell = inbox(&app, &alice).await;
    assert_eq!(bell["unread"], 1, "{bell:#}");

    // ...unless she opts in.
    let mut prefs = app
        .expect(
            get("/api/v1/notifications/preferences", Some(&alice)),
            StatusCode::OK,
        )
        .await
        .json();
    prefs["notifyOwnChanges"] = json!(true);
    app.expect(
        put("/api/v1/notifications/preferences", Some(&alice), prefs),
        StatusCode::OK,
    )
    .await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key2}"),
            Some(&alice),
            json!({ "assigneeId": null }),
        ),
        StatusCode::OK,
    )
    .await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key2}"),
            Some(&alice),
            json!({ "assigneeId": alice_id }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    assert_eq!(inbox(&app, &alice).await["unread"], 2);
}

#[tokio::test]
async fn watchers_hear_comments_and_mentions_are_addressed_by_username() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "W", "blank").await;
    let (bob_id, bob) = user(&app, &admin, "bob", "member").await;
    let (carol_id, carol) = user(&app, &admin, "carol", "member").await;
    grant(&app, &admin, "W", &bob_id, "member").await;
    grant(&app, &admin, "W", &carol_id, "member").await;
    let key = card(&app, &admin, &p, "Watched").await;
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&bob)),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;

    app.expect(
        post(
            &format!("/api/v1/cards/{key}/comments"),
            Some(&admin),
            json!({ "body": "Thoughts, @carol? (cc alice@example.com)" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    drain(&app).await;

    let bobs = inbox(&app, &bob).await;
    assert_eq!(bobs["items"][0]["kind"], "commented", "{bobs}");
    let carols = inbox(&app, &carol).await;
    assert_eq!(carols["items"][0]["kind"], "mentioned", "{carols}");
    assert!(
        carols["items"][0]["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Thoughts")
    );
}

#[tokio::test]
async fn a_burst_on_one_card_coalesces_into_one_unread_notice() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "B", "blank").await;
    let (dan_id, dan) = user(&app, &admin, "dan", "member").await;
    grant(&app, &admin, "B", &dan_id, "member").await;
    let key = card(&app, &admin, &p, "Busy").await;
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&dan)),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;

    for n in 0..4 {
        app.expect(
            patch(
                &format!("/api/v1/cards/{key}"),
                Some(&admin),
                json!({ "summary": format!("Busy {n}") }),
            ),
            StatusCode::OK,
        )
        .await;
        drain(&app).await;
    }
    let bell = inbox(&app, &dan).await;
    assert_eq!(bell["unread"], 1, "{bell}");
    assert_eq!(bell["items"][0]["count"], 4);

    // Once read, the next change starts a fresh notice.
    app.expect(
        post_empty("/api/v1/notifications/read-all", Some(&dan)),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "summary": "Quiet" }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    let bell = inbox(&app, &dan).await;
    assert_eq!(bell["unread"], 1);
    assert_eq!(bell["items"][0]["count"], 1);
}

#[tokio::test]
async fn someone_who_lost_access_hears_nothing_and_off_means_off() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "X", "blank").await;
    let (eve_id, eve) = user(&app, &admin, "eve", "member").await;
    grant(&app, &admin, "X", &eve_id, "member").await;
    let key = card(&app, &admin, &p, "Private").await;
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&eve)),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;

    app.expect(
        delete(
            &format!("/api/v1/projects/X/members/{eve_id}"),
            Some(&admin),
        ),
        StatusCode::NO_CONTENT,
    )
    .await;
    app.expect(
        post(
            &format!("/api/v1/cards/{key}/comments"),
            Some(&admin),
            json!({ "body": "secret" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    drain(&app).await;
    assert_eq!(inbox(&app, &eve).await["unread"], 0);

    // A second user turns comments off.
    let (fay_id, fay) = user(&app, &admin, "fay", "member").await;
    grant(&app, &admin, "X", &fay_id, "member").await;
    app.expect(
        post_empty(&format!("/api/v1/cards/{key}/watchers"), Some(&fay)),
        StatusCode::OK,
    )
    .await;
    let mut prefs = app
        .expect(
            get("/api/v1/notifications/preferences", Some(&fay)),
            StatusCode::OK,
        )
        .await
        .json();
    for entry in prefs["channels"].as_array_mut().expect("channels") {
        if entry["kind"] == "commented" {
            entry["channel"] = json!("off");
        }
    }
    app.expect(
        put(
            "/api/v1/notifications/preferences",
            Some(&fay),
            prefs.clone(),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    app.expect(
        post(
            &format!("/api/v1/cards/{key}/comments"),
            Some(&admin),
            json!({ "body": "again" }),
        ),
        StatusCode::CREATED,
    )
    .await;
    drain(&app).await;
    assert_eq!(inbox(&app, &fay).await["unread"], 0);

    // Email needs SMTP first.
    prefs["dailyDigest"] = json!(true);
    app.expect(
        put("/api/v1/notifications/preferences", Some(&fay), prefs),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

#[tokio::test]
async fn you_can_only_read_your_own_notifications() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "R", "blank").await;
    let (gus_id, gus) = user(&app, &admin, "gus", "member").await;
    let (_, hal) = user(&app, &admin, "hal", "member").await;
    grant(&app, &admin, "R", &gus_id, "member").await;
    let key = card(&app, &admin, &p, "Mine").await;
    drain(&app).await;
    app.expect(
        patch(
            &format!("/api/v1/cards/{key}"),
            Some(&admin),
            json!({ "assigneeId": gus_id }),
        ),
        StatusCode::OK,
    )
    .await;
    drain(&app).await;
    let id = text(&inbox(&app, &gus).await["items"][0], "id");
    app.expect(
        post_empty(&format!("/api/v1/notifications/{id}/read"), Some(&hal)),
        StatusCode::NOT_FOUND,
    )
    .await;
    app.expect(
        post_empty(&format!("/api/v1/notifications/{id}/read"), Some(&gus)),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(inbox(&app, &gus).await["unread"], 0);
}

#[tokio::test]
async fn the_feed_sees_writes_that_bypass_the_api_and_carries_the_actor() {
    // The MCP subprocess and the webhook write the database directly; the feed must see them.
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "F", "blank").await;
    let key = card(&app, &admin, &p, "Fed").await;
    drain(&app).await;

    let found = atlas::domain::card::find_by_key(&app.db, &key)
        .await
        .expect("query")
        .expect("card");
    atlas::activity::with_actor("agent:session-1".to_owned(), async {
        let mut tx = app.db.begin_write().await.expect("tx");
        atlas::domain::comment::insert(
            &mut tx,
            &found.id,
            &found.creator_id,
            "from an agent",
            atlas::test_support::now(),
        )
        .await
        .expect("insert");
        tx.commit().await.expect("commit");
    })
    .await;

    let events = atlas::activity::read_new(&app.db).await.expect("read");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor.as_deref(), Some("agent:session-1"));
    assert_eq!(events[0].kind.name(), "comment.added");
    // Read once means read: the cursor moved.
    assert_eq!(
        atlas::activity::read_new(&app.db)
            .await
            .expect("read")
            .len(),
        0
    );
}

#[tokio::test]
async fn live_frames_reach_only_subscribers_who_can_see_the_project() {
    let app = App::new().await;
    let admin = admin_past_the_gate(&app).await;
    let p = project(&app, &admin, "L", "blank").await;
    let state = app.state();
    let mut feed = state.hub.subscribe();
    let key = card(&app, &admin, &p, "Live").await;
    jobs::pump_until_idle(&state).await.expect("pump");

    let message = feed.try_recv().expect("a frame was published");
    assert_eq!(message.payload["type"], "card.created");
    assert_eq!(message.payload["cardKey"], key.as_str());
    assert_eq!(message.project_id.as_deref(), Some(p.id.as_str()));
}
