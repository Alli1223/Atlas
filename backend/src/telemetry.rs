//! Tracing setup and the HTTP request span.

use axum::extract::Request;
use axum::http::HeaderName;
use tracing::Span;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

use crate::config::{Config, LogFormat};

/// The header carrying the request id, in and out.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Installs the global tracing subscriber.
///
/// `RUST_LOG` wins over `ATLAS_LOG_LEVEL` when set, matching the convention
/// every other Rust tool follows.
///
/// # Errors
///
/// If `ATLAS_LOG_LEVEL` is not a valid filter directive.
pub fn init(config: &Config) -> anyhow::Result<()> {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::try_new(&config.log_level)?,
    };

    // Both arms must have one type, hence the boxing. The fmt layer is added
    // before the filter so that it is a `Layer<Registry>`, which is the type the
    // box is erased to.
    let fmt_layer: Box<dyn Layer<Registry> + Send + Sync> = match config.log_format() {
        LogFormat::Json => Box::new(
            fmt::layer()
                .json()
                // Keep the request span's fields (notably request_id) on every
                // event inside it — without this a log shipper cannot correlate
                // lines back to a request.
                .with_current_span(true)
                .with_span_list(true),
        ),
        LogFormat::Pretty => Box::new(fmt::layer().pretty().with_ansi(true)),
    };

    Registry::default().with(fmt_layer).with(filter).init();

    Ok(())
}

/// Reads the request id from the request headers.
///
/// The id is read back out of the header rather than generated here, so that it
/// matches the one the client is handed back. That requires `SetRequestIdLayer`
/// to sit *outside* the `TraceLayer` — see [`crate::api::middleware`].
///
/// Returns `"-"` when there is no id, or when a client sent one that is not
/// printable ASCII. A log line is never worth a panic.
fn request_id_of(request: &Request) -> &str {
    request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-")
}

/// Builds the span covering one HTTP request.
pub fn make_request_span(request: &Request) -> Span {
    let request_id = request_id_of(request);

    tracing::info_span!(
        "http",
        method = %request.method(),
        // `uri` can carry a query string; treat it as operator data, not as
        // something to parse or index.
        uri = %request.uri(),
        version = ?request.version(),
        request_id = %request_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    fn request_with(headers: &[(&str, &[u8])]) -> Request {
        let mut builder = HttpRequest::builder().uri("/healthz");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn the_request_id_is_read_from_the_header() {
        let request = request_with(&[("x-request-id", b"abc-123")]);
        assert_eq!(request_id_of(&request), "abc-123");
    }

    #[test]
    fn a_missing_request_id_falls_back_to_a_placeholder() {
        let request = request_with(&[]);
        assert_eq!(request_id_of(&request), "-");
    }

    #[test]
    fn a_non_ascii_request_id_falls_back_rather_than_panicking() {
        // A client controls this header, so it must not be trusted to be
        // printable. `to_str` fails on non-visible-ASCII bytes.
        let request = request_with(&[("x-request-id", &[0xff, 0xfe])]);
        assert_eq!(request_id_of(&request), "-");
    }

    #[test]
    fn the_request_span_is_named_http() {
        let request = request_with(&[("x-request-id", b"abc-123")]);
        let span = make_request_span(&request);
        assert_eq!(span.metadata().expect("span has metadata").name(), "http");
    }

    #[test]
    fn the_configured_log_level_must_be_a_valid_filter() {
        assert!(EnvFilter::try_new(&Config::default().log_level).is_ok());
    }
}
