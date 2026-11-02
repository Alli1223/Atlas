//! The tower layer stack applied to every route.

use std::any::Any;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower::ServiceBuilder;
use tower_http::LatencyUnit;
use tower_http::catch_panic::{CatchPanicLayer, ResponseForPanic};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};

use crate::config::Config;
use crate::error::{AppError, Problem};
use crate::telemetry::{self, REQUEST_ID_HEADER};

/// How long a request may run before it is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The floor of the outermost request-body limit, in bytes.
///
/// The layer itself is set to the larger of this and `ATLAS_MAX_UPLOAD_MB` (see
/// [`body_limit`]), because an attachment upload is the one body that may legitimately be
/// large. That does not loosen anything else: every `Json`/`Bytes` extractor carries axum's
/// own 2 MB `DefaultBodyLimit`, and the upload handlers count the bytes they stream to disk
/// against the configured limit themselves.
const BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// The outermost body limit for a configuration.
fn body_limit(config: &Config) -> usize {
    usize::try_from(config.max_upload_bytes())
        .unwrap_or(usize::MAX)
        .max(BODY_LIMIT_BYTES)
}

/// How long a browser may cache a CORS preflight.
const CORS_MAX_AGE: Duration = Duration::from_mins(10);

/// Wraps `router` in the standard middleware stack.
///
/// Layer order is load-bearing. `ServiceBuilder` applies layers top-down, so the
/// first listed is the outermost — a request passes through them in written
/// order and the response comes back up in reverse:
///
/// 1. **`SetRequestIdLayer`** — must be outermost so that everything below,
///    including the trace span, sees the id.
/// 2. **`TraceLayer`** — spans every request; reads the id set above.
/// 3. **`PropagateRequestIdLayer`** — copies the id onto the response, so a
///    user reporting a failure can quote a value that is in the logs.
/// 4. **`CatchPanicLayer`** — turns a panicking handler into a 500 instead of
///    killing the connection. Below tracing so panics are still traced.
/// 5. **`CorsLayer`** — must answer preflights before the timeout and body
///    limit can reject them.
/// 6. **`TimeoutLayer`**, **`RequestBodyLimitLayer`** — resource guards.
/// 7. **`CompressionLayer`** — innermost of the tower layers, so it compresses
///    the final body.
/// 8. **`problem_instance`** — fills in the RFC 7807 `instance` field.
///
/// `problem_instance` is applied in a *separate* `Router::layer` call rather
/// than inside the `ServiceBuilder`, and that is not stylistic.
/// `RequestBodyLimitLayer` rewrites the request body type to
/// `Limited<Body>`, and everything nested inside it must therefore be generic
/// over the body. `axum::middleware::from_fn` is not — it only ever accepts
/// `Request<Body>` — so placing it inside the stack fails to compile. Each
/// `Router::layer` call re-boxes the result into `Route`, which *is* generic
/// over the body, which restores the property the next layer needs.
pub fn apply<S>(router: Router<S>, config: &Config) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let stack = ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(telemetry::make_request_span)
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::INFO)
                        .latency_unit(LatencyUnit::Millis),
                ),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(CatchPanicLayer::custom(PanicAsProblem))
        .layer(cors_layer(config))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(RequestBodyLimitLayer::new(body_limit(config)))
        .layer(CompressionLayer::new());

    let headers = Arc::new(security_headers(config));
    router
        .layer(axum::middleware::from_fn(problem_instance))
        .layer(axum::middleware::map_response(
            move |mut response: Response| {
                let headers = Arc::clone(&headers);
                async move {
                    for (name, value) in headers.iter() {
                        response
                            .headers_mut()
                            .entry(name.clone())
                            .or_insert_with(|| value.clone());
                    }
                    response
                }
            },
        ))
        .layer(axum::Extension(TrustedProxies::parse(
            &config.trusted_proxies,
        )))
        .layer(stack)
}

/// Which peers' `X-Forwarded-For` / `X-Real-IP` headers are believed
/// (`ATLAS_TRUSTED_PROXIES`). Read by [`crate::auth::extract::ClientInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedProxies {
    /// `*`: any peer — the documented single-reverse-proxy deploy.
    Any,
    /// `none`: never; the TCP peer is the client.
    None,
    /// Only these peers, and only these hops are skipped when reading the chain.
    List(Vec<IpAddr>),
}

impl TrustedProxies {
    /// Parses the setting. Unparseable entries are dropped with a warning; a list that ends
    /// up empty trusts nobody, which fails safe.
    pub fn parse(setting: &str) -> Self {
        match setting.trim() {
            "" | "*" => Self::Any,
            s if s.eq_ignore_ascii_case("none") => Self::None,
            list => Self::List(
                list.split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .filter_map(|entry| {
                        entry.parse().ok().or_else(|| {
                            tracing::warn!(
                                entry,
                                "ignoring an unparseable ATLAS_TRUSTED_PROXIES entry"
                            );
                            None
                        })
                    })
                    .collect(),
            ),
        }
    }

    /// Whether a peer may speak for the client.
    pub fn trusts(&self, peer: Option<IpAddr>) -> bool {
        match self {
            Self::Any => true,
            Self::None => false,
            Self::List(list) => peer.is_some_and(|ip| list.contains(&ip)),
        }
    }

    /// Whether a hop in the forwarded chain is one of ours (and so skipped).
    pub fn is_proxy(&self, hop: &str) -> bool {
        match self {
            Self::Any | Self::None => false,
            Self::List(list) => hop.parse::<IpAddr>().is_ok_and(|ip| list.contains(&ip)),
        }
    }
}

/// The SHA-256 hashes (CSP source form) of every inline `<script>` in a page.
fn inline_script_hashes(html: &str) -> Vec<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        let body_start = tag_end + 1;
        let Some(close) = after[body_start..].find("</script>") else {
            break;
        };
        let body = &after[body_start..body_start + close];
        if !tag.contains("src=") && !body.trim().is_empty() {
            let digest = Sha256::digest(body.as_bytes());
            hashes.push(format!(
                "'sha256-{}'",
                base64::engine::general_purpose::STANDARD.encode(digest)
            ));
        }
        rest = &after[body_start + close..];
    }
    hashes
}

/// The headers every response carries unless a handler set its own.
///
/// The CSP allows exactly what the SPA needs: its own origin, the hashes of the inline
/// no-flash theme script in the served `index.html` (computed at startup, so editing that
/// script cannot silently break the policy), inline styles (React's `style` props), images
/// from `data:`/`blob:` (pasted attachments, previews), and the live WebSocket. HSTS is sent
/// only when Atlas knows it is served over HTTPS (`ATLAS_PUBLIC_URL`), because on plain HTTP
/// it would be ignored at best and a lockout at worst once TLS is added on another port.
fn security_headers(config: &Config) -> Vec<(HeaderName, HeaderValue)> {
    let hashes = config
        .static_dir
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join("index.html")).ok())
        .map(|html| inline_script_hashes(&html))
        .unwrap_or_default();
    let csp = format!(
        "default-src 'self'; script-src 'self'{}{}; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; \
         object-src 'none'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        if hashes.is_empty() { "" } else { " " },
        hashes.join(" ")
    );
    let mut headers = vec![
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ),
        (
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
        ),
    ];
    if let Ok(value) = HeaderValue::from_str(&csp) {
        headers.push((header::CONTENT_SECURITY_POLICY, value));
    }
    if config
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://"))
    {
        headers.push((
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));
    }
    headers
}

/// Builds the CORS layer from configuration.
///
/// Note the coupling between credentials and origins: the CORS spec forbids
/// `Access-Control-Allow-Origin: *` together with credentials, and Atlas
/// authenticates with cookies (Phase 2). So `*` and cookie auth are mutually
/// exclusive, and choosing `*` silently disables credentialed requests. That is
/// worth a warning rather than a surprise.
fn cors_layer(config: &Config) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            REQUEST_ID_HEADER,
        ])
        .expose_headers([REQUEST_ID_HEADER])
        .max_age(CORS_MAX_AGE);

    if config.cors_allows_any_origin() {
        tracing::warn!(
            "CORS is configured to allow any origin; cookie authentication will not work \
             cross-origin because the CORS specification forbids credentials with a wildcard \
             origin. Set ATLAS_CORS_ALLOWED_ORIGINS to an explicit list."
        );
        return layer.allow_origin(AllowOrigin::any());
    }

    let mut origins = Vec::new();
    for origin in config.cors_origins() {
        match HeaderValue::from_str(origin) {
            Ok(value) => origins.push(value),
            // Do not fail startup over one bad entry, but do not silently drop
            // it either: a typo'd origin looks exactly like a CORS bug later.
            Err(err) => tracing::warn!(
                origin,
                error = %err,
                "ignoring unparseable entry in ATLAS_CORS_ALLOWED_ORIGINS"
            ),
        }
    }

    layer
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
}

/// Fills in the RFC 7807 `instance` field with the request path.
///
/// [`IntoResponse for AppError`](crate::error::AppError) cannot do this itself:
/// it never sees the request. So it stashes the [`Problem`] in the response
/// extensions and this layer, which does have the URI, completes and re-renders
/// it. Rebuilding from the extension avoids parsing the JSON body back out.
pub async fn problem_instance(request: Request, next: Next) -> Response {
    let instance = request.uri().path().to_owned();

    let mut response = next.run(request).await;

    if let Some(problem) = response.extensions_mut().remove::<Problem>() {
        return problem.with_instance(instance).into_response();
    }

    response
}

/// Renders a panicking handler as a problem document rather than a bare 500.
#[derive(Debug, Clone, Copy)]
struct PanicAsProblem;

impl ResponseForPanic for PanicAsProblem {
    type ResponseBody = Body;

    fn response_for_panic(
        &mut self,
        err: Box<dyn Any + Send + 'static>,
    ) -> Response<Self::ResponseBody> {
        // `panic!` payloads are &str for literals and String for formatted
        // messages; anything else is opaque.
        let message = if let Some(s) = err.downcast_ref::<&'static str>() {
            (*s).to_owned()
        } else if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_owned()
        };

        tracing::error!(panic = %message, "handler panicked");

        // The panic message may contain anything at all, so it goes to the log
        // and the client gets the generic internal-error document.
        AppError::internal(anyhow::anyhow!("handler panicked: {message}")).into_response()
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn inline_scripts_are_hashed_and_external_ones_are_not() {
        let html = "<head><script>\n  var a = 1\n</script><script type=\"module\" src=\"/x.js\"></script></head>";
        let hashes = inline_script_hashes(html);
        assert_eq!(hashes.len(), 1);
        assert!(hashes[0].starts_with("'sha256-"));
    }

    #[test]
    fn trusted_proxies_parse() {
        assert_eq!(TrustedProxies::parse("*"), TrustedProxies::Any);
        assert_eq!(TrustedProxies::parse("none"), TrustedProxies::None);
        let list = TrustedProxies::parse("127.0.0.1, ::1, junk");
        assert!(list.trusts(Some("127.0.0.1".parse().unwrap())));
        assert!(list.trusts(Some("::1".parse().unwrap())));
        assert!(!list.trusts(Some("10.0.0.9".parse().unwrap())));
        assert!(!list.trusts(None));
        assert!(list.is_proxy("127.0.0.1"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use tower::ServiceExt;

    /// A named fn rather than a closure: `panic!` alone makes the return type
    /// `!`, which the `Handler` trait cannot infer a response type from.
    async fn boom() -> &'static str {
        panic!("this should not reach the client")
    }

    fn test_router(config: &Config) -> Router {
        let router = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route("/boom", get(boom))
            .route("/missing", get(|| async { AppError::NotFound }));
        apply(router, config)
    }

    #[tokio::test]
    async fn a_request_id_is_generated_and_echoed_back() {
        let response = test_router(&Config::default())
            .oneshot(
                HttpRequest::builder()
                    .uri("/ok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let id = response
            .headers()
            .get("x-request-id")
            .expect("x-request-id must be echoed back");
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn a_client_supplied_request_id_is_preserved() {
        // Losing an inbound id breaks correlation across a proxy.
        let response = test_router(&Config::default())
            .oneshot(
                HttpRequest::builder()
                    .uri("/ok")
                    .header("x-request-id", "client-supplied-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("x-request-id").unwrap(),
            "client-supplied-id"
        );
    }

    #[tokio::test]
    async fn a_panic_becomes_a_problem_document_and_does_not_leak_the_message() {
        let response = test_router(&Config::default())
            .oneshot(
                HttpRequest::builder()
                    .uri("/boom")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );

        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 500);
        assert!(
            !json
                .to_string()
                .contains("this should not reach the client"),
            "panic message leaked to the client: {json}"
        );
    }

    #[tokio::test]
    async fn problem_instance_is_filled_in_with_the_request_path() {
        let response = test_router(&Config::default())
            .oneshot(
                HttpRequest::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["instance"], "/missing");
        assert_eq!(json["type"], "urn:atlas:error:not-found");
    }

    #[tokio::test]
    async fn preflight_is_allowed_for_a_configured_origin() {
        let config = Config {
            cors_allowed_origins: "http://localhost:5173".to_owned(),
            ..Config::default()
        };

        let response = test_router(&config)
            .oneshot(
                HttpRequest::builder()
                    .method(Method::OPTIONS)
                    .uri("/ok")
                    .header("origin", "http://localhost:5173")
                    .header("access-control-request-method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "http://localhost:5173"
        );
        // Cookie auth depends on this being set.
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-credentials")
                .unwrap(),
            "true"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_origin_is_not_allowed() {
        let config = Config {
            cors_allowed_origins: "http://localhost:5173".to_owned(),
            ..Config::default()
        };

        let response = test_router(&config)
            .oneshot(
                HttpRequest::builder()
                    .uri("/ok")
                    .header("origin", "https://evil.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none(),
            "an unconfigured origin must not be granted CORS access"
        );
    }

    #[tokio::test]
    async fn an_oversized_body_is_rejected_from_its_content_length_alone() {
        // The valuable path: rejected on the header, before a single byte of the
        // body is read.
        let router = apply(
            Router::new().route("/echo", axum::routing::post(|body: String| async { body })),
            &Config::default(),
        );

        let oversized = body_limit(&Config::default()) + 1;
        let response = router
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/echo")
                    .header(header::CONTENT_LENGTH, oversized)
                    // The header alone must trigger the refusal, so there is no need to
                    // allocate the oversized body itself.
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn a_body_within_the_limit_is_accepted() {
        // Guards against the limit being set to something absurd like 0.
        let router = apply(
            Router::new().route("/echo", axum::routing::post(|body: String| async { body })),
            &Config::default(),
        );

        let response = router
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/echo")
                    .body(Body::from("a reasonable payload"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
