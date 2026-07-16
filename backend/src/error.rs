//! The error taxonomy, and its RFC 7807 `application/problem+json` rendering.
//!
//! One rule dominates this module: **an internal error is logged in full and
//! reported opaquely**. SQL text, file paths and anyhow chains are operator
//! information, not client information.

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::rank::RankError;

/// Shorthand for a handler result.
pub type AppResult<T> = Result<T, AppError>;

/// Every way an Atlas request can fail.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The request was well-formed but semantically invalid. 422.
    #[error("validation failed: {0}")]
    Validation(String),

    /// The addressed resource does not exist. 404.
    #[error("not found")]
    NotFound,

    /// The request conflicts with current state (stale rank, duplicate key). 409.
    #[error("conflict: {0}")]
    Conflict(String),

    /// No credentials, or bad credentials. 401.
    #[error("unauthorized")]
    Unauthorized,

    /// Authenticated, but not allowed. 403.
    #[error("forbidden")]
    Forbidden,

    /// The request itself is malformed. 400.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Anything unexpected. Logged in full, reported opaquely. 500.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// Builds an [`AppError::Internal`] from any error.
    pub fn internal(err: impl Into<anyhow::Error>) -> Self {
        Self::Internal(err.into())
    }

    /// The HTTP status this error maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable machine-readable error identifier, used as the problem `type`.
    ///
    /// A URN rather than an `https://` URL: Atlas is self-hosted and has no
    /// canonical public documentation host to point at.
    fn type_urn(&self) -> &'static str {
        match self {
            Self::Validation(_) => "urn:atlas:error:validation",
            Self::NotFound => "urn:atlas:error:not-found",
            Self::Conflict(_) => "urn:atlas:error:conflict",
            Self::Unauthorized => "urn:atlas:error:unauthorized",
            Self::Forbidden => "urn:atlas:error:forbidden",
            Self::BadRequest(_) => "urn:atlas:error:bad-request",
            Self::Internal(_) => "urn:atlas:error:internal",
        }
    }

    /// Short, human-readable summary. Stable for a given `type`.
    fn title(&self) -> &'static str {
        match self {
            Self::Validation(_) => "Validation Failed",
            Self::NotFound => "Not Found",
            Self::Conflict(_) => "Conflict",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::BadRequest(_) => "Bad Request",
            Self::Internal(_) => "Internal Server Error",
        }
    }

    /// Client-safe explanation.
    ///
    /// `Internal` deliberately discards the cause: it has already been logged.
    fn detail(&self) -> String {
        match self {
            Self::Validation(msg) | Self::Conflict(msg) | Self::BadRequest(msg) => msg.clone(),
            Self::NotFound => "The requested resource does not exist.".to_owned(),
            Self::Unauthorized => "Authentication is required to access this resource.".to_owned(),
            Self::Forbidden => "You do not have permission to perform this action.".to_owned(),
            Self::Internal(_) => {
                "An unexpected internal error occurred. The incident has been logged.".to_owned()
            }
        }
    }

    /// Renders this error as an RFC 7807 problem document.
    ///
    /// `instance` is filled in later by
    /// [`crate::api::middleware::problem_instance`], which is the only layer
    /// that can see the request URI.
    pub fn problem(&self) -> Problem {
        Problem {
            problem_type: self.type_urn().to_owned(),
            title: self.title().to_owned(),
            status: self.status().as_u16(),
            detail: self.detail(),
            instance: None,
        }
    }
}

/// An RFC 7807 problem document.
///
/// Serialised as `application/problem+json`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Problem {
    /// URI identifying the error type, e.g. `urn:atlas:error:not-found`.
    #[serde(rename = "type")]
    #[schema(rename = "type", example = "urn:atlas:error:not-found")]
    pub problem_type: String,

    /// Short, human-readable summary, stable per `type`.
    #[schema(example = "Not Found")]
    pub title: String,

    /// The HTTP status code, repeated here per RFC 7807.
    #[schema(example = 404)]
    pub status: u16,

    /// Human-readable explanation specific to this occurrence.
    pub detail: String,

    /// URI reference identifying the specific occurrence — the request path.
    pub instance: Option<String>,
}

impl Problem {
    /// Attaches the request path as the problem `instance`.
    #[must_use]
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let body = match serde_json::to_vec(&self) {
            Ok(body) => body,
            Err(err) => {
                // Serialising a Problem cannot realistically fail, but a panic
                // in the error path is the worst possible failure mode.
                tracing::error!(error = ?err, "failed to serialise problem document");
                br#"{"type":"urn:atlas:error:internal","title":"Internal Server Error","status":500,"detail":"An unexpected internal error occurred.","instance":null}"#.to_vec()
            }
        };

        let response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/problem+json")
            .body(Body::from(body));

        match response {
            Ok(mut response) => {
                // Stash the document so `problem_instance` can fill in `instance`
                // without having to parse the body back out.
                response.extensions_mut().insert(self);
                response
            }
            Err(err) => {
                tracing::error!(error = ?err, "failed to build problem response");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // The full chain goes to the operator; `detail()` goes to the client.
        if let Self::Internal(err) = &self {
            tracing::error!(error = ?err, "internal server error");
        }
        self.problem().into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            // A missing row is a 404, not a 500 — every other database error is
            // ours to fix, so it must not be described to the client.
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Internal(anyhow::Error::new(other)),
        }
    }
}

impl From<garde::Report> for AppError {
    fn from(report: garde::Report) -> Self {
        Self::Validation(report.to_string())
    }
}

impl From<RankError> for AppError {
    fn from(err: RankError) -> Self {
        match err {
            // The client's neighbours moved: it must refetch and retry.
            RankError::OutOfOrder { .. } => Self::Conflict(err.to_string()),
            // A corrupt rank in the database is our bug, not the client's.
            RankError::Decode(_) => Self::Internal(anyhow::Error::new(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn statuses_map_as_documented() {
        assert_eq!(AppError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(AppError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(AppError::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            AppError::Conflict("x".into()).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            AppError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Validation("x".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            AppError::internal(anyhow::anyhow!("x")).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn problem_json_has_the_rfc_7807_shape_and_content_type() {
        let response = AppError::NotFound.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );

        let json = body_json(response).await;
        assert_eq!(json["type"], "urn:atlas:error:not-found");
        assert_eq!(json["title"], "Not Found");
        assert_eq!(json["status"], 404);
        assert!(json["detail"].is_string());
        // `instance` is present but null until the middleware fills it in.
        assert!(json.get("instance").is_some());
    }

    #[tokio::test]
    async fn internal_errors_never_leak_their_cause() {
        let secret = "SELECT password_hash FROM users WHERE token = 'hunter2'";
        let response = AppError::internal(anyhow::anyhow!(secret)).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let json = body_json(response).await;
        let rendered = json.to_string();
        assert!(!rendered.contains("password_hash"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert_eq!(json["title"], "Internal Server Error");
        assert_eq!(
            json["detail"],
            "An unexpected internal error occurred. The incident has been logged."
        );
    }

    #[tokio::test]
    async fn client_facing_errors_do_carry_their_message() {
        let response = AppError::Conflict("neighbours reordered; refetch".into()).into_response();
        let json = body_json(response).await;
        assert_eq!(json["detail"], "neighbours reordered; refetch");
    }

    #[test]
    fn row_not_found_becomes_a_404_but_other_db_errors_do_not() {
        assert!(matches!(
            AppError::from(sqlx::Error::RowNotFound),
            AppError::NotFound
        ));
        assert!(matches!(
            AppError::from(sqlx::Error::PoolTimedOut),
            AppError::Internal(_)
        ));
    }

    #[test]
    fn a_stale_rank_is_a_conflict_not_a_500() {
        let err = RankError::OutOfOrder {
            before: "80".into(),
            after: "80".into(),
        };
        assert!(matches!(AppError::from(err), AppError::Conflict(_)));
    }

    #[test]
    fn instance_is_attached_by_with_instance() {
        let problem = AppError::NotFound
            .problem()
            .with_instance("/api/v1/cards/1");
        assert_eq!(problem.instance.as_deref(), Some("/api/v1/cards/1"));
    }
}
