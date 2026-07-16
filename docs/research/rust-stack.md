# Rust backend stack (2026): Axum + SQLx + SQLite for a Jira-clone with subprocess supervision and WebSocket streaming

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

All versions below were verified against the crates.io API and docs.rs on 2026-07-16, not recalled. Two releases postdate my training and change the plan materially: **sqlx 0.9.0** (2026-05-21) adds a `SqlSafeStr` bound to every `query*()` function (dynamic SQL now needs `AssertSqlSafe`), makes the `sqlite` feature imply *bundled* plus `load-extension`/`deserialize`/`unlock-notify`, raises MSRV to **1.94.0**, adds an `sqlx.toml` config, and breaks `cargo install --locked sqlx-cli`; and **aes-gcm/chacha20poly1305 0.11.0** (2026-06-28) moved to `aead` 0.6, replacing `generate_key(&mut OsRng)` with `Key::<C>::generate()`/`Nonce::generate()`. Recommended stack: axum 0.8.9 (`{id}` route syntax, not `:id`), sqlx 0.9.0 with `sqlite-bundled` (FTS5 is compiled in via `-DSQLITE_ENABLE_FTS5`), a **split writer(1)/reader(N) pool** since sqlx exposes no `BEGIN IMMEDIATE` option (use the verified `Pool::begin_with("BEGIN IMMEDIATE")`), **session cookies over JWT** for the SPA, XChaCha20Poly1305 + HKDF for API-key encryption, `fractional_index` 2.0.2 for drag-and-drop ordering, and garde 0.23 + utoipa 5.5 for validation/OpenAPI. Two dependency traps found: `tower-sessions-sqlx-store` 0.15.0 is unresolvable against the current stack (pins sqlx ^0.8 **and** `tower-sessions-core ^0.14` while `tower-sessions` 0.15 pins core `=0.15.0`), and argon2's `DEFAULT_M_COST` is **19456 KiB**, not the "19" the docs prose implies.

## Implementation notes

## 0. Cargo.toml (all versions verified 2026-07-16)

Requires **Rust >= 1.94.0** (sqlx 0.9 MSRV). Pin it in `rust-toolchain.toml`.

```toml
[package]
name = "atlas"
edition = "2024"
rust-version = "1.94"

[dependencies]
axum = { version = "0.8.9", features = ["ws", "macros"] }
axum-extra = { version = "0.12.6", features = ["cookie-private", "typed-header"] }
tokio = { version = "1.52.4", features = ["full"] }
tower = "0.5.3"
tower-http = { version = "0.7.0", features = [
  "trace", "cors", "request-id", "timeout", "limit", "sensitive-headers", "catch-panic", "normalize-path",
] }
# NOTE: `sqlite-bundled`, NOT `sqlite` — in 0.9 `sqlite` also pulls load-extension/deserialize/unlock-notify.
sqlx = { version = "0.9.0", default-features = false, features = [
  "runtime-tokio", "tls-none", "sqlite-bundled", "macros", "migrate", "uuid", "chrono", "json",
] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
thiserror = "2.0.18"
uuid = { version = "1.24.0", features = ["v4", "v7", "serde"] }
chrono = { version = "0.4.45", features = ["serde"] }
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter", "json"] }
argon2 = { version = "0.5.3", features = ["std"] }   # 0.6 is still an RC
chacha20poly1305 = { version = "0.11.0", features = ["getrandom"] }
hkdf = "0.13.0"
sha2 = "0.10"
zeroize = { version = "1.9.0", features = ["derive"] }
subtle = "2"
base64 = "0.22.1"
fractional_index = { version = "2.0.2", features = ["serde"] }
garde = { version = "0.23.0", features = ["derive", "email", "url"] }
utoipa = { version = "5.5.0", features = ["axum_extras", "uuid", "chrono", "macros"] }
utoipa-axum = "0.2.0"
utoipa-swagger-ui = { version = "9.0.2", features = ["axum"] }
futures = "0.3.32"

[target.'cfg(unix)'.dependencies]
nix = { version = "0.31.3", features = ["signal"] }

[dev-dependencies]
axum-test = "21.0.0"
```

Deliberately **excluded**: `tower-sessions` + `tower-sessions-sqlx-store` (the store pins sqlx ^0.8 and tower-sessions-core ^0.14 vs tower-sessions 0.15's core `=0.15.0` — unresolvable); `jsonwebtoken` (see §4); `validator` (stale vs garde).

## 1. Axum 0.8: router, state, AppError

Route params are `{id}` — `:id` is a **runtime panic** in 0.8.

```rust
#[derive(Clone)]
struct AppState {
    reader: SqlitePool,          // N connections, read_only
    writer: SqlitePool,          // exactly 1 connection
    cookie_key: axum_extra::extract::cookie::Key,
    crypto: Arc<Crypto>,
    jobs: Arc<JobRegistry>,
}
// Sub-state so handlers can take `State<Key>` for PrivateCookieJar:
impl FromRef<AppState> for Key { fn from_ref(s: &AppState) -> Key { s.cookie_key.clone() } }

let app = Router::new()
    .route("/api/issues/{id}", get(get_issue).patch(patch_issue))  // {} not :
    .route("/api/jobs/{id}/stream", any(ws_stream))
    .layer(TraceLayer::new_for_http())
    .layer(SetSensitiveRequestHeadersLayer::new([header::COOKIE, header::AUTHORIZATION]))
    .with_state(state);
```

AppError — one enum, `IntoResponse`, `#[from]` conversions so handlers use `?`. Critical rule: **log the internal cause, return an opaque body** so SQL errors never leak to the SPA.

```rust
#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("not found")]                 NotFound,
    #[error("unauthorized")]              Unauthorized,
    #[error("forbidden")]                 Forbidden,
    #[error("conflict: {0}")]             Conflict(String),
    #[error("validation failed")]         Validation(#[from] garde::Report),
    #[error(transparent)]                 Db(#[from] sqlx::Error),
    #[error(transparent)]                 Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::NotFound      => (StatusCode::NOT_FOUND, "not found".into()),
            AppError::Unauthorized  => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            AppError::Forbidden     => (StatusCode::FORBIDDEN, "forbidden".into()),
            AppError::Conflict(m)   => (StatusCode::CONFLICT, m.clone()),
            AppError::Validation(r) => (StatusCode::UNPROCESSABLE_ENTITY, r.to_string()),
            // RowNotFound is a 404, everything else is a 500 with an opaque body.
            AppError::Db(sqlx::Error::RowNotFound) => (StatusCode::NOT_FOUND, "not found".into()),
            e => {
                tracing::error!(error = ?e, "internal error");   // full detail to logs only
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}
pub type AppResult<T> = Result<T, AppError>;
```

Auth as an extractor (`FromRequestParts`) keeps handlers honest — an endpoint that forgets auth won't compile if it needs `CurrentUser`.

## 2. SQLx 0.9: the SqlSafeStr change

The big 0.9 break. `query()` now takes `impl SqlSafeStr`, implemented only for `&'static str` and `AssertSqlSafe`:

```rust
sqlx::query("SELECT 1").execute(&pool).await?;                 // OK: &'static str
let sql = format!("SELECT * FROM t ORDER BY {col}");
sqlx::query(&sql)                                              // ❌ no longer compiles
sqlx::query(sqlx::AssertSqlSafe(sql))                          // ✅ explicit opt-in
```

Use it only for a **validated allowlist** (e.g. mapping a sort field to a hardcoded column), never for user input. Prefer the macros, which verify against the real schema at compile time:

```rust
let issue = sqlx::query_as!(Issue, r#"SELECT id, title, rank as "rank!" FROM issues WHERE id = ?"#, id)
    .fetch_one(&state.reader).await?;
```

SQLite typing tips: use `as "x!"` to force non-null, `as "n: i64"` to override. `query_as!` needs field order to match the SELECT.

**Offline / CI** (mandatory here, since the macros otherwise need a live DB):
```bash
cargo install sqlx-cli --version 0.9.0 --no-default-features --features sqlite  # NO --locked in 0.9
export DATABASE_URL="sqlite://./atlas.db?mode=rwc"
sqlx database create && sqlx migrate run
cargo sqlx prepare -- --all-targets   # writes .sqlx/ -> COMMIT IT
```
CI sets `SQLX_OFFLINE=true` and adds `cargo sqlx prepare --check` to catch a stale `.sqlx/`.

Migrations: `sqlx migrate add -r create_issues`, then embed:
```rust
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
MIGRATOR.run(&writer).await?;   // run on the WRITER pool, at startup, before serving
```

## 3. SQLite tuning: why two pools

WAL gives **one writer + N concurrent readers**. sqlx's `begin()` emits `BEGIN DEFERRED`, so a txn that reads then writes tries to upgrade and can get `SQLITE_BUSY_SNAPSHOT` — which `busy_timeout` does **not** retry (it's an immediate failure, not a busy-wait). sqlx 0.9 exposes no transaction-behavior setting. Two mitigations, use both:

1. **Writer pool of exactly 1** — serializes writes in-process, so upgrade conflicts can't happen between your own connections.
2. **`pool.begin_with("BEGIN IMMEDIATE")`** — verified to exist and be ungated; takes the write lock upfront so `busy_timeout` *does* apply.

```rust
fn base(path: &str) -> SqliteConnectOptions {
    SqliteConnectOptions::from_str(path).unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)     // default is Delete
        .synchronous(SqliteSynchronous::Normal)   // default FULL; NORMAL is the right WAL tradeoff
        .busy_timeout(Duration::from_secs(10))    // default 5s
        .foreign_keys(true)                       // sqlx already defaults ON — be explicit
        .pragma("cache_size", "-64000")           // 64 MiB, negative = KiB
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "268435456")
        .optimize_on_close(true, Some(400));
}

// ONE writer. WAL allows only one anyway; a bigger pool just converts serialization into SQLITE_BUSY.
let writer = SqlitePoolOptions::new().max_connections(1)
    .connect_with(base(url)).await?;

// N readers. read_only(true) makes accidental writes fail loudly rather than lock-contend.
let reader = SqlitePoolOptions::new().max_connections(8)
    .connect_with(base(url).read_only(true)).await?;
```
`synchronous = NORMAL` in WAL risks losing the *last few* committed txns on OS/power loss (not corruption) — the standard, accepted web-app tradeoff. Route every read to `reader` and every write to `writer`; the type system won't enforce it, so wrap them in a small repo layer.

## 4. Auth: session cookies, not JWT

**Recommendation: opaque session cookie + server-side session table.** Justification, specific to a browser SPA:
- **Revocation.** Jira-clone semantics (remove user from project, force logout, rotate on password change) need instant invalidation. A JWT is valid until it expires; you'd need a server-side denylist — at which point you have session lookups anyway, minus the simplicity.
- **XSS.** A JWT the SPA can read (localStorage) is exfiltratable by any XSS. `HttpOnly` cookies are not readable from JS. JWT-in-a-cookie is fine, but then you're doing cookies *and* carrying JWT's revocation problem.
- **CSRF.** Solved by `SameSite=Lax` (+ Origin check on mutations, and `SameSite=None; Secure` only if the API is cross-site). Lax already blocks cross-site POST.
- **Refresh.** Sliding expiry is a single `UPDATE sessions SET expires_at=...` — no refresh-token rotation/replay machinery.

Use JWT only for stateless service-to-service or the subprocess-agent callback. If you do: `jsonwebtoken` 10.4.0 enables **no crypto backend by default** (`default = ["use_pem"]`) — you must add `features = ["aws_lc_rs"]` or `["rust_crypto"]`.

Cookies: **axum-extra `PrivateCookieJar`** (feature `cookie-private`, AEAD-encrypted+signed). Skip `tower-sessions` — its SQLite store is unresolvable against this stack (§facts), and the session table is ~40 lines.

```rust
// Session id: 256 bits of CSPRNG, stored HASHED (sha256) so a DB leak isn't a login.
// Compare with subtle::ConstantTimeEq.
let jar = jar.add(
    Cookie::build(("sid", session_id_b64))
        .http_only(true).secure(true).same_site(SameSite::Lax)
        .path("/").max_age(time::Duration::days(14)).build(),
);
```
Password hashing (note the KiB unit — `19` would be 19 KiB, not 19 MiB):
```rust
use argon2::{Argon2, Algorithm, Version, Params, PasswordHasher, PasswordVerifier,
             password_hash::{SaltString, PasswordHash, rand_core::OsRng}}; // rand_core 0.6 OsRng!

let params = Params::new(19_456, 2, 1, None)?;  // == Params::DEFAULT (19 MiB / t=2 / p=1), OWASP
let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
let hash = argon2.hash_password(pw.as_bytes(), &SaltString::generate(&mut OsRng))?.to_string();
// verify (~40ms) — run under tokio::task::spawn_blocking so it doesn't stall the reactor
argon2.verify_password(pw.as_bytes(), &PasswordHash::new(&stored)?).is_ok()
```
Always hash a dummy password on unknown-user to avoid a timing oracle.

## 5. Encryption at rest for API keys

**Crate: `chacha20poly1305` 0.11 (XChaCha20Poly1305).** Over aes-gcm: the 192-bit nonce is safe to pick randomly forever (birthday bound is irrelevant), whereas AES-GCM's 96-bit nonce makes random selection a real reuse risk at scale, and AES without hardware AES-NI is both slower and harder to keep constant-time. Over `ring`: worse ergonomics, no zeroize integration. **Note the aead 0.6 API change (June 2026)** — `generate_key(&mut OsRng)` is gone.

**KDF: HKDF-SHA256, not Argon2.** Argon2 is for *low-entropy* passwords; a 32-byte master key from env/secret-manager is already uniform, so a memory-hard KDF buys nothing and just adds latency. HKDF gives per-purpose subkeys from one master. (`keyring` 4.1.5 is for desktop apps — a server daemon has no logged-in session/DBus, so use env + a real secret manager.)

```rust
// ATLAS_MASTER_KEY = base64(32 random bytes)
pub struct Crypto { cipher: XChaCha20Poly1305 }

impl Crypto {
    pub fn from_master(master: &[u8]) -> anyhow::Result<Self> {
        let hk = Hkdf::<Sha256>::new(Some(b"atlas.v1"), master);   // salt = version -> rotatable
        let mut key = Zeroizing::new([0u8; 32]);
        hk.expand(b"api-key-encryption", key.as_mut())?;            // per-purpose subkey
        Ok(Self { cipher: XChaCha20Poly1305::new(key.as_ref().into()) })
    }

    pub fn seal(&self, pt: &Secret<String>, aad: &[u8]) -> anyhow::Result<Encrypted> {
        let nonce = XNonce::generate();                             // aead 0.6 API
        let ct = self.cipher.encrypt(&nonce, Payload { msg: pt.expose().as_bytes(), aad })?;
        Ok(Encrypted { nonce: nonce.to_vec(), ct })                 // store nonce||ct as BLOB
    }
}
```
Bind AAD to the row identity (e.g. `format!("user:{user_id}:cred:{cred_id}")`) so a ciphertext can't be copy-pasted onto another row.

**Un-loggable wrapper.** The point is that `Debug`/`Display`/`Serialize` are all dead ends, so `tracing::info!(?secret)` cannot leak it:

```rust
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    pub fn new(v: T) -> Self { Self(v) }
    pub fn expose(&self) -> &T { &self.0 }   // grep-able name = audit point
}
impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Secret([REDACTED])") }
}
impl<T: Zeroize> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("[REDACTED]") }
}
impl<T: Zeroize> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("refusing to serialize a Secret"))  // fails loudly
    }
}
impl<'de, T: Zeroize + Deserialize<'de>> Deserialize<'de> for Secret<T> { /* inbound is fine */ }
```
`secrecy` 0.10.3 gives you this off the shelf; hand-rolling is worth it for the deliberate `Serialize` rejection. Caveat: `Zeroize` on `String`/`Vec` can't scrub earlier reallocations — construct secrets directly into the wrapper.

## 6. Subprocess supervision

Three separate problems: orphans, backpressure, and fan-out.

```rust
let mut child = Command::new(&cmd)
    .args(&args)
    .stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null())
    .kill_on_drop(true)     // backstop only — reaps the direct child, NOT descendants
    .process_group(0)       // Unix: new process group, PGID == child PID  <-- the orphan fix
    .spawn()?;
let pgid = Pid::from_raw(child.id().ok_or(...)? as i32);
```
Without `process_group(0)` the child shares *your* group: a `npm test` that spawns workers leaves them running when you kill the child (they reparent to init and keep holding ports/CPU). With it, you signal the whole tree:

```rust
// Graceful: SIGTERM the GROUP (note the killpg, not child.kill() — that misses grandchildren)
signal::killpg(pgid, Signal::SIGTERM).ok();
match timeout(Duration::from_secs(10), child.wait()).await {
    Ok(status) => status?,
    Err(_) => { signal::killpg(pgid, Signal::SIGKILL).ok(); child.wait().await? }  // escalate
}
```
Always `child.wait()` after killing, or you leave a zombie.

Streaming + fan-out to N sockets via `broadcast` (each subscriber gets every message; slow ones get `RecvError::Lagged` rather than stalling the producer — exactly the backpressure policy you want for log tailing):

```rust
let (tx, _rx) = broadcast::channel::<Arc<LogLine>>(1024);
let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
tokio::spawn(async move {
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(Arc::new(LogLine { seq, stream: Stdout, text: line }));  // Err = no subs, fine
        // also append to DB so late joiners can backfill
    }
});
```
Use `Arc<LogLine>` — broadcast clones per subscriber. Ring-buffer the last N lines (e.g. `VecDeque` under a `Mutex`) so a client that connects mid-job gets history, then live tail:

```rust
async fn ws_stream(ws: WebSocketUpgrade, State(s): State<AppState>, Path(id): Path<Uuid>) -> Response {
    ws.on_upgrade(move |socket| async move {
        let (mut sink, mut recv) = socket.split();
        let mut rx = s.jobs.subscribe(id);
        // 1) replay backlog, 2) then live:
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(l)  => { if sink.send(Message::Text(json.into())).await.is_err() { break } }
                    Err(RecvError::Lagged(n)) => { /* tell client it skipped n lines */ }
                    Err(RecvError::Closed) => break,
                },
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    if sink.send(Message::Ping(Bytes::new())).await.is_err() { break }  // proxies kill idle conns
                }
                Some(Ok(_)) = recv.next() => { /* client msg / close */ }
            }
        }
    })
}
```
`Message::Text` takes `Utf8Bytes` in 0.8 (`.into()` from String). Keep a `JobRegistry: DashMap<Uuid, JobHandle>` holding the sender + a kill channel; on graceful shutdown, iterate and killpg every job.

## 7. Ordering: fractional indexing over LexoRank

**Recommend `fractional_index` 2.0.2.** LexoRank (Jira's own) is a *bucketed* scheme whose rebalancing is exactly the operational pain you want to avoid; there's no maintained Rust LexoRank crate anyway (`lexorank` 2.0.0: 2023, ~21k downloads). Fractional indexing generates a key strictly between two neighbors, so a reorder is **one UPDATE of one row** — no neighbor rewrites, no reindex job.

```rust
// Reorder = compute a key between the two rows the card was dropped between.
let rank = match (prev, next) {
    (Some(a), Some(b)) => FractionalIndex::new_between(&a, &b)
        .ok_or(AppError::Conflict("neighbours reordered; refetch".into()))?,  // None if a>=b
    (None,    Some(b)) => FractionalIndex::new_before(&b),
    (Some(a), None)    => FractionalIndex::new_after(&a),
    (None,    None)    => FractionalIndex::default(),
};
sqlx::query!("UPDATE issues SET rank = ? WHERE id = ?", rank.to_string(), id)
    .execute(&state.writer).await?;
```
Store `rank` as **TEXT** and sort `ORDER BY rank, id` (id as a stable tiebreak). Because `to_string()` is hex-encoded order-preserving, SQLite's default BINARY collation sorts it correctly with no custom collation — but the column must **not** be `COLLATE NOCASE`.

**The rebalancing problem:** keys grow ~1 char per insert at the same spot. Repeatedly dropping into the same gap ("insert always at position 2") grows keys linearly — thousands of ops before it matters, but it's unbounded. Mitigations, in order: (a) ignore it — 99% of boards never notice; (b) monitor `MAX(LENGTH(rank))` and if it crosses ~50, run an offline rebalance that rewrites the column with evenly spaced keys (single txn on the writer pool, bump a `board.rank_version` so clients refetch); (c) `new_between` returning `None` is your concurrency signal — two clients dropping into the same gap concurrently is fine (both get distinct keys), but a stale client's neighbors may have moved, so return 409 and let it refetch. Fractional indexing also has a known "interleaving" hazard under concurrent edits — irrelevant for a server-authoritative Jira clone with one DB.

## 8. Validation / DTOs / OpenAPI

**garde 0.23 over validator 0.20** — actively maintained (2026-05 vs 2025-01), better context/nested support. Wire with `axum-valid` 0.25 (supports axum 0.8 + garde 0.23).

```rust
#[derive(Deserialize, garde::Validate, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]           // typo'd field = 422, not silently ignored
pub struct CreateIssue {
    #[garde(length(chars, min = 1, max = 200))] pub title: String,
    #[garde(skip)]                              pub description: Option<String>,
    #[garde(dive)]                              pub priority: Priority,
}
async fn create(State(s): State<AppState>, Garde(Json(body)): Garde<Json<CreateIssue>>) -> AppResult<Json<Issue>> { ... }
```
serde patterns: separate `CreateX`/`UpdateX`/`XResponse` from the DB row (never expose `password_hash` by deriving Serialize on the entity); `#[serde(rename_all = "camelCase")]` for a TS client; for PATCH, distinguish absent from null with `Option<Option<T>>` + `#[serde(default, skip_serializing_if = "Option::is_none")]`.

**OpenAPI: utoipa 5.5 + utoipa-axum 0.2** (aide 0.15 is 16x less used and its 0.16 is alpha). `utoipa-axum`'s `OpenApiRouter` derives the spec from the actual routes, so the spec can't drift:
```rust
let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
    .routes(routes!(create_issue, get_issue))
    .split_for_parts();
let router = router.merge(SwaggerUi::new("/swagger").url("/api-docs/openapi.json", api));
```
CI: dump the spec to a file, run `openapi-typescript`, and `git diff --exit-code` to fail when the frontend client is stale.

## 9. Testing

`#[sqlx::test]` on SQLite needs no DATABASE_URL — it makes an isolated DB per test at `target/sqlx/test-dbs/`, auto-runs `./migrations`, and cleans up on success (failures keep the DB for post-mortem). Combine with `axum-test` 21.

```rust
#[sqlx::test(fixtures("users", "projects"))]
async fn reorder_persists_and_is_idempotent(pool: SqlitePool) -> anyhow::Result<()> {
    let server = TestServer::new(app(test_state(pool.clone())))?;
    let res = server.post("/api/auth/login").json(&json!({"email":"a@b.c","password":"pw"})).await;
    res.assert_status_ok();
    let res = server.patch("/api/issues/1/rank").json(&json!({"after": 2})).await;
    res.assert_status_ok();
    // assert through the PUBLIC API, then verify persistence in the DB
    let ids: Vec<i64> = sqlx::query_scalar!("SELECT id FROM issues ORDER BY rank, id").fetch_all(&pool).await?;
    assert_eq!(ids, vec![2, 1, 3]);
    Ok(())
}
```
A good integration test here: drives real HTTP through the real router (auth middleware included), asserts the response *and* the DB state, and covers the failure path (401/403/409). For the subprocess supervisor, test against a real short-lived process (`sh -c 'echo a; sleep 5'`) and assert that killing the job leaves **no** orphans (`killpg(pgid, None)` should return ESRCH). `axum-test` uses reqwest ^0.13 — it can drive real WebSockets.

## 10. Tracing

```rust
tracing_subscriber::registry()
    .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,atlas=debug,sqlx=warn".into()))
    .with(fmt::layer().json().with_span_events(FmtSpan::CLOSE))   // json() in prod, pretty in dev
    .init();
```
Request ids via tower-http's `request-id` feature (pulls uuid) — `tower-request-id` 0.3.0 is dead (2023). Order matters: set the id **before** trace, propagate **after**:
```rust
.layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
.layer(TraceLayer::new_for_http().make_span_with(|r: &Request<_>| {
    let id = r.headers().get("x-request-id").and_then(|v| v.to_str().ok()).unwrap_or("-");
    tracing::info_span!("http", method=%r.method(), uri=%r.uri(), request_id=%id)
}))
.layer(PropagateRequestIdLayer::x_request_id())
```
Add `#[tracing::instrument(skip(pool, password))]` on handlers — **always skip secrets**; the `Secret<T>` redacted Debug from §5 is the backstop for when someone forgets.

## 11. FTS5 search

FTS5 ships with `sqlite-bundled` (verified: build.rs passes `-DSQLITE_ENABLE_FTS5`), no extra feature needed. Use an **external-content** table so text isn't duplicated, synced by triggers:

```sql
CREATE VIRTUAL TABLE issues_fts USING fts5(
  title, description,
  content='issues', content_rowid='id',
  tokenize='porter unicode61 remove_diacritics 2'
);
CREATE TRIGGER issues_ai AFTER INSERT ON issues BEGIN
  INSERT INTO issues_fts(rowid, title, description) VALUES (new.id, new.title, new.description);
END;
CREATE TRIGGER issues_ad AFTER DELETE ON issues BEGIN
  INSERT INTO issues_fts(issues_fts, rowid, title, description) VALUES('delete', old.id, old.title, old.description);
END;
CREATE TRIGGER issues_au AFTER UPDATE ON issues BEGIN
  INSERT INTO issues_fts(issues_fts, rowid, title, description) VALUES('delete', old.id, old.title, old.description);
  INSERT INTO issues_fts(rowid, title, description) VALUES (new.id, new.title, new.description);
END;
```
```rust
// bm25() weights title above body; negate for DESC-by-relevance since bm25 is negative-better.
let rows = sqlx::query_as!(Hit, r#"
    SELECT i.id, i.title, snippet(issues_fts, 1, '<mark>', '</mark>', '…', 20) AS "snippet!"
    FROM issues_fts f JOIN issues i ON i.id = f.rowid
    WHERE issues_fts MATCH ? AND i.project_id = ?
    ORDER BY bm25(issues_fts, 10.0, 1.0) LIMIT 50"#, query, project_id)
    .fetch_all(&state.reader).await?;
```
**Never pass raw user input as the MATCH pattern** — FTS5 has its own query syntax (`NEAR`, `*`, `"`, `OR`, `-`), so a stray quote is a 500 and a stray `OR` changes semantics. Sanitize by tokenizing on non-alphanumerics and re-quoting each term: `terms.map(|t| format!("\"{}\"", t.replace('"', "\"\""))).join(" ")`, appending `*` for prefix search. Run `INSERT INTO issues_fts(issues_fts) VALUES('optimize')` periodically on the writer pool.

## Facts

- **[verified]** axum 0.8.9 is the current release (published 2026-04-14). No 0.9 exists. Depends on http ^1.0, tower ^0.5.2, matchit =0.8.4; WebSockets need the `ws` feature (pulls tokio-tungstenite ^0.29).
  - Evidence: crates.io API /api/v1/crates/axum and /axum/0.8.9/dependencies
- **[verified]** axum 0.8 route params use `{id}` / wildcard `{*rest}`, NOT the 0.7 `:id` / `*rest`. Old syntax is rejected at runtime unless you call `Router::without_v07_checks()`.
  - Evidence: https://docs.rs/axum/0.8.9/axum/routing/struct.Router.html — 'Paths can contain segments like /{key}'; 'Turn off checks for compatibility with route matching syntax from 0.7'
- **[verified]** axum 0.8 `Message::Text` carries `Utf8Bytes`, not `String` (changed from 0.7). Split with `StreamExt::split` for concurrent read/write.
  - Evidence: https://docs.rs/axum/0.8.9/axum/extract/ws/index.html
- **[verified]** sqlx 0.9.0 is current (2026-05-21); sqlx-cli 0.9.0 matches. MSRV raised to 1.94.0.
  - Evidence: crates.io API; sqlx CHANGELOG 0.9.0 ('MSRV increased to 1.94.0')
- **[verified]** BREAKING (sqlx 0.9): all `query*()` functions take `impl SqlSafeStr`, implemented only for `&'static str` and the wrapper `AssertSqlSafe`. Exact spelling is `sqlx::AssertSqlSafe` — defined as `pub struct AssertSqlSafe<T>(pub T)`, wrapping &str/String/Box<str>/Arc<str>/Cow<'static,str>. (The changelog prose says 'AssertSafeSQL'; that name does not exist.)
  - Evidence: https://docs.rs/sqlx/0.9.0/sqlx/struct.AssertSqlSafe.html returns 200; sqlx/sql_str/... paths 404. Trait at https://docs.rs/sqlx/0.9.0/sqlx/trait.SqlSafeStr.html
- **[verified]** BREAKING (sqlx 0.9): the `sqlite` feature now expands to ['sqlite-bundled','sqlite-deserialize','sqlite-load-extension','sqlite-unlock-notify']. Prefer `sqlite-bundled` alone to avoid pulling unsafe extension loading. `sqlite-unbundled` exists for system SQLite (needs bindgen).
  - Evidence: crates.io /api/v1/crates/sqlx/0.9.0 features map
- **[verified]** SQLite extension loading is now `unsafe` in 0.9; SqliteValue is !Sync and SqliteValueRef is !Send to prevent data races.
  - Evidence: sqlx CHANGELOG 0.9.0 [#3928]
- **[verified]** CI trap: `cargo install --locked sqlx-cli` no longer works in 0.9 because Cargo.lock was removed from tracking. Use `cargo install sqlx-cli --version 0.9.0 --no-default-features --features sqlite` (without --locked).
  - Evidence: sqlx CHANGELOG 0.9.0 [#3821]
- **[verified]** Offline compile-time verification: `cargo sqlx prepare` writes the `.sqlx/` dir (commit it); builds use it when `SQLX_OFFLINE=true`. Requires the `macros` feature, which enables `sqlx-core/offline`.
  - Evidence: sqlx 0.9.0 feature map shows macros -> ['derive','sqlx-macros/macros','sqlx-core/offline', ...]; CHANGELOG 0.9.0 offline section
- **[verified]** sqlx 0.9 adds an `sqlx.toml` config (feature `sqlx-toml`, non-default for the library, default for sqlx-cli) supporting type overrides and relocating the migrations table.
  - Evidence: sqlx CHANGELOG 0.9.0 [#3383]; feature map has sqlx-toml
- **[verified]** SqliteConnectOptions API is unchanged in 0.9: .journal_mode(SqliteJournalMode::Wal), .busy_timeout(Duration), .foreign_keys(bool), .synchronous(SqliteSynchronous::Normal), .create_if_missing(bool), .pragma(k,v), .read_only(bool), .optimize_on_close(bool, Option<u32>). Defaults: foreign_keys ON, busy_timeout 5s, synchronous FULL, create_if_missing false.
  - Evidence: https://docs.rs/sqlx/0.9.0/sqlx/sqlite/struct.SqliteConnectOptions.html
- **[verified]** sqlx 0.9 has NO SqliteTransactionBehavior enum and no `immediate` option on SqliteConnectOptions — `pool.begin()` issues BEGIN DEFERRED, so a read txn upgrading to a write can hit SQLITE_BUSY_SNAPSHOT, which busy_timeout does NOT retry.
  - Evidence: docs.rs 404 for sqlx/sqlite/enum.SqliteTransactionBehavior.html; no 'immediate' text on SqliteConnectOptions page
- **[verified]** `Pool::begin_with(statement: impl SqlSafeStr) -> Result<Transaction<'static, DB>>` and `try_begin_with` exist and are NOT feature-gated (docs.rs mislabels them 'Available on crate feature any only' — a rendering artifact). Source shows a plain `pub async fn`. So `pool.begin_with("BEGIN IMMEDIATE")` works, and the &'static str satisfies SqlSafeStr.
  - Evidence: https://docs.rs/sqlx-core/0.9.0/src/sqlx_core/pool/mod.rs.html lines 390-405 — no #[cfg(feature = "any")] on the fn
- **[verified]** FTS5 is available out of the box with `sqlite-bundled`: libsqlite3-sys' bundled build.rs passes -DSQLITE_ENABLE_FTS5 (also FTS3, FTS3_PARENTHESIS, DBSTAT_VTAB, COLUMN_METADATA).
  - Evidence: https://docs.rs/crate/libsqlite3-sys/0.38.1/source/build.rs — .flag("-DSQLITE_ENABLE_FTS5")
- **[verified]** sqlx-sqlite 0.9.0 requires libsqlite3-sys >=0.30.1, <0.38.0, so it resolves to 0.37.x even though 0.38.1 is published. 0.9 policy: the max of that range may rise in any backwards-compatible release.
  - Evidence: crates.io /api/v1/crates/sqlx-sqlite/0.9.0/dependencies; CHANGELOG [#3928]
- **[verified]** #[sqlx::test] on SQLite needs no DATABASE_URL; it creates an isolated per-test DB at target/sqlx/test-dbs/<path>.sqlite, auto-runs ./migrations, and cleans up on success (failed tests keep their DB for debugging). Args may be SqlitePool, PoolConnection<Sqlite>, or (PoolOptions<Sqlite>, SqliteConnectOptions). Fixtures: #[sqlx::test(fixtures("users","posts"))] -> ./fixtures/{name}.sql, applied in order. Needs `macros` + `migrate`.
  - Evidence: https://docs.rs/sqlx/0.9.0/sqlx/attr.test.html
- **[verified]** argon2 0.5.3 is the current STABLE (0.6.0-rc.8 is an RC, published 2026-04-21). 0.5.3 uses password-hash ^0.5 (whose rand_core is 0.6) — so `SaltString::generate` needs `argon2::password_hash::rand_core::OsRng`, NOT rand 0.9/0.10's OsRng.
  - Evidence: crates.io /api/v1/crates/argon2 and /argon2/0.5.3/dependencies
- **[verified]** SECURITY-CRITICAL: argon2's `Params::DEFAULT_M_COST: u32 = 19 * 1024` = 19456 (KiB, i.e. 19 MiB). DEFAULT_T_COST=2, DEFAULT_P_COST=1, DEFAULT_OUTPUT_LEN=32. Docs prose renders this as '19 MiB' — passing `19` to Params::new would mean 19 KiB and be catastrophically weak. Matches OWASP (19 MiB/2/1).
  - Evidence: https://docs.rs/argon2/0.5.3/src/argon2/params.rs.html line 42: `pub const DEFAULT_M_COST: u32 = 19 * 1024;`
- **[verified]** DEPENDENCY TRAP: tower-sessions-sqlx-store 0.15.0 (last updated 2025-01-01) depends on sqlx ^0.8.0 (incompatible with sqlx 0.9) AND tower-sessions-core ^0.14.0, while tower-sessions 0.15.0 pins tower-sessions-core =0.15.0. ^0.14 excludes 0.15, so the two cannot co-resolve. Do not plan on it.
  - Evidence: crates.io /api/v1/crates/tower-sessions-sqlx-store/0.15.0/dependencies and /tower-sessions/0.15.0/dependencies
- **[verified]** axum-extra 0.12.6 works with axum ^0.8.9 and provides cookie jars via features `cookie`, `cookie-signed`, `cookie-private` (cookie ^0.18), plus `typed-header`. tower-cookies 0.11.0 is an alternative but is older (2025-01-01).
  - Evidence: crates.io /api/v1/crates/axum-extra/0.12.6 features + dependencies
- **[verified]** jsonwebtoken 10.4.0: `default = ["use_pem"]` only — NO crypto backend is enabled by default. You must opt into `aws_lc_rs` or `rust_crypto`. (This is a change from the 8.x/9.x ring-based design.)
  - Evidence: crates.io /api/v1/crates/jsonwebtoken/10.4.0 feature map + dependency list (no `ring` dep)
- **[verified]** BREAKING (aead 0.6, 2026-06-28): chacha20poly1305 0.11.0 and aes-gcm 0.11.0 changed key/nonce generation to `Key::<C>::generate()` and `Nonce::generate()` via new `Generate`/`AeadCore` traits, replacing 0.10's `C::generate_key(&mut OsRng)` / `C::generate_nonce(&mut OsRng)`. Requires the `getrandom` feature. Cipher construction is still `C::new(&key)` via KeyInit; encrypt/decrypt still `(&nonce, data) -> Result`.
  - Evidence: https://docs.rs/chacha20poly1305/0.11.0/chacha20poly1305/ usage examples; aes-gcm 0.11.0 deps show aead ^0.6, cipher ^0.5
- **[verified]** tokio::process::Command has `pub fn process_group(&mut self, pgroup: i32) -> &mut Command` (Unix only); `process_group(0)` makes the child's PGID equal its own PID. `child.kill()` signals only the direct child — the docs do not claim it reaches descendants, so grandchildren orphan unless you killpg.
  - Evidence: https://docs.rs/tokio/1.52.4/tokio/process/struct.Command.html
- **[verified]** kill_on_drop(true) reaps only the direct child, and on Unix killed processes become zombies until reaped ('best-effort basis' by the runtime) — it is not a substitute for process-group cleanup.
  - Evidence: https://docs.rs/tokio/1.52.4/tokio/process/struct.Command.html
- **[verified]** nix 0.31.3 is current; `killpg` needs feature `signal` (which enables `process`). Alternative: rustix 1.1.4.
  - Evidence: crates.io /api/v1/crates/nix/0.31.3 feature map
- **[verified]** fractional_index 2.0.2 is byte-string based (not float), implements Ord (direct binary comparison), and its to_string() hex form preserves lexicographic order for non-Rust consumers. API: `FractionalIndex::default()`, `new_between(a,b) -> Option<Self>` (None if a==b or out of order), `new_before(&a) -> Self`, `new_after(&a) -> Self`, `new(Option,Option) -> Option<Self>`, `to_string()`, `from_string(&str) -> Result<Self, DecodeError>`. Serde on by default; use `#[serde(with="fractional_index::stringify")]` for JSON hex. Only dep is optional serde.
  - Evidence: https://docs.rs/fractional_index/2.0.2/fractional_index/struct.FractionalIndex.html
- **[verified]** Alternative ordering crates are weak: `lexorank` 2.0.0 last published 2023-04-27 with only ~21.5k lifetime downloads; `mudder` 0.1.5 has ~5.1k. fractional_index has ~210k. No maintained LexoRank crate exists.
  - Evidence: crates.io API download counts and updated_at for lexorank/mudder/fractional_index
- **[verified]** garde 0.23.0 (2026-05-23, actively maintained) vs validator 0.20.0 (2025-01-20, stale). axum-valid 0.25.0 (2026-06-30) supports axum ^0.8 and BOTH garde ^0.23 and validator ^0.20. No `garde-axum` crate exists.
  - Evidence: crates.io /api/v1/crates/garde, /validator, /axum-valid/0.25.0/dependencies
- **[verified]** utoipa 5.5.0 is current (no 6.x). utoipa-axum 0.2.0 supports axum ^0.8.0 + utoipa ^5.0.0; utoipa-swagger-ui 9.0.2 has an optional axum ^0.8.0 feature; utoipa-scalar 0.3.0 / utoipa-redoc 6.0.0 also exist. Relevant utoipa features: axum_extras, macros, chrono, uuid, time, preserve_order, yaml.
  - Evidence: crates.io API for utoipa 5.5.0 features; utoipa-axum/0.2.0 and utoipa-swagger-ui/9.0.2 dependencies
- **[verified]** aide 0.15.1 is the stable alternative (0.16.0-alpha.4 is pre-release) but has ~2.2M downloads vs utoipa's ~35.8M. utoipa is the safer pick for frontend codegen.
  - Evidence: crates.io /api/v1/crates/aide and /utoipa
- **[verified]** tower-http 0.7.0 (2026-06-15) is compatible with axum 0.8: it depends on http ^1.0, tower-layer ^0.3.3, tower-service ^0.3, tower ^0.5. Needed features: `trace` (TraceLayer), `cors`, `request-id` (which pulls uuid), `timeout`, `limit`, `sensitive-headers`, `catch-panic`, `normalize-path`.
  - Evidence: crates.io /api/v1/crates/tower-http/0.7.0 dependencies + feature map
- **[verified]** tower-http 0.7.0 breaking changes are mostly irrelevant here: compression now returns 406 when identity is unacceptable; SizeAbove threshold u16->u64; no-op `tokio`/`async-compression` features removed; GrpcCode/GrpcFailureClass now #[non_exhaustive]; FollowRedirect forwards Extensions by default; trailing-slash file requests 404. TraceLayer/CorsLayer APIs are intact.
  - Evidence: tower-http CHANGELOG 0.7.0
- **[verified]** Current supporting versions: tokio 1.52.4, tower 0.5.3, serde 1.0.228, serde_json 1.0.150, thiserror 2.0.18, anyhow 1.0.103, uuid 1.24.0, time 0.3.53, chrono 0.4.45, tracing 0.1.44, tracing-subscriber 0.3.23, zeroize 1.9.0, secrecy 0.10.3, hkdf 0.13.0, keyring 4.1.5, axum-test 21.0.0 (axum ^0.8.9), tokio-util 0.7.18, tokio-stream 0.1.18, futures 0.3.32, base64 0.22.1.
  - Evidence: crates.io API bulk query 2026-07-16
- **[verified]** rand 0.10.2 is now the max stable (published 2026-07-02); 0.9.5 and 0.8.7 were published later (2026-07-11) as maintenance backports, which makes crates.io's `newest_version` field misleading.
  - Evidence: crates.io /api/v1/crates/rand version list
- *[likely]* command-group 5.0.1 (2023-11-18) wraps process-group spawn/kill, but is stale; tokio's built-in `process_group()` + a killpg via nix 0.31 is the leaner path.
  - Evidence: crates.io /api/v1/crates/command-group
- *[likely]* In WAL mode SQLite permits one writer concurrent with N readers; readers never block the writer and vice versa. This is what motivates a 1-connection writer pool + N-connection read_only pool.
  - Evidence: SQLite WAL semantics (sqlite.org/wal.html); corroborated by sqlx's lack of a BEGIN IMMEDIATE option

## Risks

- sqlx 0.9.0 is ~2 months old (2026-05-21) and is a large breaking release (SqlSafeStr, sqlx.toml, Migrate trait changes). Most blog posts/LLM output still target 0.8 and will not compile. If you hit blockers, sqlx 0.8.6 is the conservative fallback — but note that most third-party sqlx integrations (incl. tower-sessions-sqlx-store) still pin ^0.8, so 0.9 buys you compile-time safety at the cost of ecosystem lag.
- sqlx 0.9 requires Rust >= 1.94.0. Verify your CI toolchain and any distro-pinned rustc before committing to it; pin via rust-toolchain.toml.
- argon2 0.5.3's Params::new takes m_cost in KiB. Passing `19` (from the docs' '19 MiB' prose) instead of 19456 yields a ~1000x weaker hash that would pass every test silently. Assert Params::DEFAULT_M_COST == 19456 in a unit test.
- argon2 0.5.3 pulls password-hash 0.5 -> rand_core 0.6, while the rest of the tree may use rand 0.9/0.10 (rand_core 0.9). You MUST use argon2::password_hash::rand_core::OsRng for SaltString::generate; the two OsRng types are distinct and the error is confusing. argon2 0.6.0-rc.8 fixes this but is an RC — don't ship it.
- aead 0.6 (chacha20poly1305/aes-gcm 0.11, released 2026-06-28) changed key/nonce generation to Key::generate()/Nonce::generate(). Any AI-generated or pre-mid-2026 snippet will use generate_key(&mut OsRng) and won't compile. If you'd rather not chase a 3-week-old API, 0.10.x is the proven fallback.
- tower-sessions-sqlx-store 0.15.0 cannot resolve against this stack (pins sqlx ^0.8 AND tower-sessions-core ^0.14 while tower-sessions 0.15 pins core =0.15.0) and is 18 months stale. Do not budget on it; hand-roll the session table.
- The writer/reader pool split is a convention the compiler cannot enforce — one handler that writes via `state.reader` fails at runtime (read_only) or, worse, a read on `state.writer` silently contends the single connection. Wrap the pools in newtypes or a repository layer, and never expose the raw SqlitePool in AppState.
- Argon2 verification is ~40ms of CPU by design; calling it directly in an async handler blocks a tokio worker thread and login becomes a trivial DoS. It MUST go in spawn_blocking.
- process_group(0) is Unix-only. If Windows support is ever needed, that whole supervision path needs a Job Object implementation — gate it behind a trait now rather than retrofitting.
- kill_on_drop(true) is not orphan prevention: it reaps only the direct child, and on Unix leaves zombies until reaped 'best-effort'. Without explicit killpg + wait(), a `npm test`-style child's workers survive job cancellation and hold ports/CPU indefinitely.
- SQLite WAL + synchronous=NORMAL can lose the last few committed transactions on power/OS failure (not corruption). Fine for a Jira clone; confirm it's acceptable before shipping, and note WAL requires the DB on a local filesystem — it breaks on NFS and on many container bind-mount setups.
- FTS5 external-content tables desync silently if any write bypasses the triggers (e.g. a bulk migration or a manual UPDATE). The index goes stale with no error. Add a periodic 'integrity-check' command and rebuild with INSERT INTO issues_fts(issues_fts) VALUES('rebuild').
- Passing unsanitized user input to FTS5 MATCH is both a 500-error source (unbalanced quotes) and a semantic hazard (bare OR/NEAR/- are operators). Always re-quote terms.
- fractional_index 2.0.2 was last published 2024-09 with modest adoption (~210k downloads). It's small, complete and dependency-free (only optional serde), so the bus-factor risk is low, but expect no upstream fixes — vendor it if that matters.
- Fractional index keys grow unboundedly under repeated insertion at the same position. Monitor MAX(LENGTH(rank)) and have the rebalance path written before you need it, not after.
- Storing the rank column with COLLATE NOCASE (or any non-BINARY collation) silently breaks the ordering guarantee, since the hex stringification relies on binary lexicographic comparison.
- broadcast::channel drops messages for lagging subscribers (RecvError::Lagged). A slow WebSocket client silently misses log lines unless you surface the gap to the UI and backfill from the DB.
- docs.rs mislabels Pool::begin_with as 'Available on crate feature any only' — I verified against sqlx-core source that it is not gated. If a build ever disagrees, fall back to the writer-pool-of-1, which makes BEGIN IMMEDIATE unnecessary for your own connections anyway.
- Committing .sqlx/ is mandatory for CI (SQLX_OFFLINE=true), and it goes stale silently whenever a query changes. Add `cargo sqlx prepare --check` to CI or builds will pass locally and fail in CI (or vice versa).
