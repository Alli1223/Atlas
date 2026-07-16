//! The tower layer stack applied to every route.

use std::any::Any;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
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

/// Maximum accepted request body, in bytes.
///
/// Attachment uploads (Phase 9) will need a larger, route-specific limit layered
/// over this one; 10 MiB is the right default for everything else.
const BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

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
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES))
        .layer(CompressionLayer::new());

    router
        .layer(axum::middleware::from_fn(problem_instance))
        .layer(stack)
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

        let oversized = BODY_LIMIT_BYTES + 1;
        let response = router
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/echo")
                    .header(header::CONTENT_LENGTH, oversized)
                    .body(Body::from(vec![b'x'; oversized]))
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
