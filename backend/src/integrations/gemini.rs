//! Google Gemini (`TODO.md` Phase 14): image generation (Nano Banana) and text, over plain
//! HTTPS. See `docs/research/gemini-api.md` — and its corrections — before changing anything
//! here; several of the rules below exist because the obvious approach is wrong.
//!
//! - **`generateContent` on `v1`**, not `v1beta/interactions`: stable channel, and its errors
//!   are a plain object (interactions wraps them in an array).
//! - **`x-goog-api-key` header**, never `?key=`: the key stays out of URLs, proxy logs and
//!   `Referer`s.
//! - **Classify errors by `details[].reason`**, not status: an invalid key is HTTP **400**
//!   `INVALID_ARGUMENT` with reason `API_KEY_INVALID` — the same status as a malformed body.
//!   A missing key is 403 with *no* `details` at all.
//! - **Retry only transient failures** (5xx), with backoff. A 429 on image generation usually
//!   means the key has *no paid image quota* (there is no free tier for images), which no
//!   amount of retrying fixes — it is surfaced, not retried forever.
//! - **A 200 can contain no image**: `promptFeedback.blockReason` (the prompt was refused) or
//!   a `finishReason` of `IMAGE_SAFETY`/`*PROHIBITED_CONTENT` (the output was). Both are a
//!   first-class [`GeminiError::Blocked`], never an `unwrap`.
//! - **No count parameter**: N options means N requests.
//! - **Model ids churn**: they live in config (`ATLAS_GEMINI_IMAGE_MODEL`,
//!   `ATLAS_GEMINI_TEXT_MODEL`), not in code.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};
use crate::secrets::Secret;
use crate::secrets::vault::{ValidationOutcome, Validator};

/// The production API host.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// What went wrong, in the terms a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiError {
    /// The key is wrong, revoked, or missing.
    InvalidKey,
    /// Rate-limited, or (for images) the key has no paid image quota.
    QuotaExceeded,
    /// Billing must be enabled for this key/region.
    BillingRequired,
    /// The request itself was rejected.
    BadRequest(String),
    /// Google had a problem; worth retrying later.
    Transient(String),
    /// The prompt or the output was blocked by a safety filter.
    Blocked(String),
}

impl GeminiError {
    /// As an [`AppError`] with a message a user can act on.
    pub fn into_app(self) -> AppError {
        match self {
            Self::InvalidKey => AppError::Conflict(
                "The Gemini API key was rejected. Check or replace it in Settings → Integrations."
                    .to_owned(),
            ),
            Self::QuotaExceeded => AppError::Conflict(
                "Gemini refused for quota. Image generation has no free tier: the key needs \
                 billing enabled, or you are being rate-limited — try again shortly."
                    .to_owned(),
            ),
            Self::BillingRequired => AppError::Conflict(
                "Gemini needs billing enabled for this key before it can generate.".to_owned(),
            ),
            Self::BadRequest(message) => {
                AppError::Validation(format!("Gemini rejected the request: {message}"))
            }
            Self::Transient(message) => AppError::Conflict(format!(
                "Gemini is having trouble right now ({message}). Try again in a minute."
            )),
            Self::Blocked(reason) => AppError::Validation(format!(
                "Gemini's safety filter blocked this ({reason}). Rephrase the prompt and try again."
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ApiError,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    status: String,
    // Absent on the 403 no-key response — must default.
    #[serde(default)]
    details: Vec<ErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    reason: Option<String>,
}

/// Classifies an error response. Public for its unit tests' sake.
pub fn classify(status: u16, body: &str) -> GeminiError {
    let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(body) else {
        return if status >= 500 {
            GeminiError::Transient(format!("HTTP {status}"))
        } else {
            GeminiError::BadRequest(format!("HTTP {status}"))
        };
    };
    let error = envelope.error;
    let reason = error.details.iter().find_map(|d| d.reason.as_deref());
    match (status, error.status.as_str(), reason) {
        (_, _, Some("API_KEY_INVALID")) | (401 | 403, _, _) => GeminiError::InvalidKey,
        (429, _, _) => GeminiError::QuotaExceeded,
        (400, "FAILED_PRECONDITION", _) => GeminiError::BillingRequired,
        (500..=599, _, _) => GeminiError::Transient(error.message),
        _ => GeminiError::BadRequest(error.message),
    }
}

/// One generated image.
#[derive(Debug, Clone)]
pub struct Image {
    /// `image/png` or `image/jpeg` — as returned, never assumed.
    pub mime_type: String,
    /// The decoded bytes.
    pub bytes: Vec<u8>,
}

/// Token usage, for cost.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    /// Prompt tokens.
    pub input_tokens: i64,
    /// Output tokens (an image counts as its token equivalent).
    pub output_tokens: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, alias = "inline_data")]
    inline_data: Option<Blob>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Blob {
    #[serde(alias = "mime_type")]
    mime_type: String,
    data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    #[serde(default)]
    block_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: i64,
    #[serde(default)]
    candidates_token_count: i64,
}

/// What a successful generate call returned.
#[derive(Debug)]
pub struct Generated {
    /// The images, in order.
    pub images: Vec<Image>,
    /// Any text parts, joined.
    pub text: String,
    /// Tokens used.
    pub usage: Usage,
}

/// Interprets a 200 body: images and text, or a safety block.
pub fn interpret(body: &str) -> Result<Generated, GeminiError> {
    let response: GenerateResponse = serde_json::from_str(body)
        .map_err(|err| GeminiError::Transient(format!("an unreadable response: {err}")))?;
    if let Some(reason) = response
        .prompt_feedback
        .as_ref()
        .and_then(|f| f.block_reason.clone())
    {
        return Err(GeminiError::Blocked(format!("prompt: {reason}")));
    }
    let mut images = Vec::new();
    let mut text = String::new();
    let mut finish = None;
    for candidate in &response.candidates {
        finish = finish.or(candidate.finish_reason.clone());
        // Never index parts[0]: text and image parts interleave.
        for part in candidate.content.iter().flat_map(|c| &c.parts) {
            if let Some(blob) = &part.inline_data {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(blob.data.as_bytes())
                    .map_err(|err| {
                        GeminiError::Transient(format!("undecodable image data: {err}"))
                    })?;
                images.push(Image {
                    mime_type: blob.mime_type.clone(),
                    bytes,
                });
            }
            if let Some(t) = &part.text {
                text.push_str(t);
            }
        }
    }
    let usage = response
        .usage_metadata
        .map_or_else(Usage::default, |u| Usage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
        });
    match finish.as_deref() {
        Some(
            reason @ ("SAFETY"
            | "IMAGE_SAFETY"
            | "PROHIBITED_CONTENT"
            | "IMAGE_PROHIBITED_CONTENT"
            | "BLOCKLIST"),
        ) if images.is_empty() => Err(GeminiError::Blocked(format!("output: {reason}"))),
        _ => Ok(Generated {
            images,
            text,
            usage,
        }),
    }
}

/// A Gemini client for one key.
#[derive(Debug)]
pub struct GeminiClient {
    http: reqwest::Client,
    base_url: String,
    key: Secret<String>,
}

/// How many attempts a transient failure gets.
const ATTEMPTS: u32 = 3;

impl GeminiClient {
    /// A client for `key` against `base_url` (default: Google's).
    pub fn new(key: Secret<String>, base_url: Option<&str>) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(AppError::internal)?;
        Ok(Self {
            http,
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_owned(),
            key,
        })
    }

    async fn post(&self, model: &str, body: &Value) -> Result<Generated, GeminiError> {
        let url = format!("{}/v1/models/{model}:generateContent", self.base_url);
        let mut delay = Duration::from_millis(500);
        let mut last = GeminiError::Transient("no attempt made".to_owned());
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
            let response = match self
                .http
                .post(&url)
                .header("x-goog-api-key", self.key.expose())
                .json(body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(err) => {
                    last = GeminiError::Transient(if err.is_timeout() {
                        "timed out".to_owned()
                    } else {
                        "unreachable".to_owned()
                    });
                    continue;
                }
            };
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            if status == 200 {
                return interpret(&text);
            }
            let error = classify(status, &text);
            match error {
                GeminiError::Transient(_) => last = error,
                other => return Err(other),
            }
        }
        Err(last)
    }

    /// Generates one image. `aspect_ratio` like `1:1`, `16:9`; `size` like `1K`.
    pub async fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        aspect_ratio: &str,
        size: &str,
    ) -> Result<Generated, GeminiError> {
        let body = json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": {
                "responseModalities": ["TEXT", "IMAGE"],
                "imageConfig": { "aspectRatio": aspect_ratio, "imageSize": size },
            },
        });
        let generated = self.post(model, &body).await?;
        if generated.images.is_empty() {
            return Err(GeminiError::Blocked("no image was returned".to_owned()));
        }
        Ok(generated)
    }

    /// Generates text. With `schema`, asks for JSON matching it.
    pub async fn generate_text(
        &self,
        model: &str,
        prompt: &str,
        schema: Option<&Value>,
    ) -> Result<Generated, GeminiError> {
        let mut body = json!({ "contents": [{ "parts": [{ "text": prompt }] }] });
        if let Some(schema) = schema {
            body["generationConfig"] =
                json!({ "responseMimeType": "application/json", "responseSchema": schema });
        }
        self.post(model, &body).await
    }

    /// The cheap, zero-token key check: `GET /v1beta/models?pageSize=1`.
    ///
    /// Proves the key is valid, **not** that it can generate images (no free tier) — the
    /// outcome says only what it learned.
    pub async fn validate(&self) -> AppResult<ValidationOutcome> {
        let url = format!("{}/v1beta/models?pageSize=1", self.base_url);
        let response = self
            .http
            .get(&url)
            .header("x-goog-api-key", self.key.expose())
            .send()
            .await
            .map_err(|err| AppError::internal(anyhow::anyhow!("Gemini is unreachable: {err}")))?;
        let status = response.status().as_u16();
        if status == 200 {
            return Ok(ValidationOutcome {
                status: crate::secrets::CredentialStatus::Valid,
                scopes: None,
                expires_at: None,
            });
        }
        let body = response.text().await.unwrap_or_default();
        match classify(status, &body) {
            GeminiError::InvalidKey => Ok(ValidationOutcome {
                status: crate::secrets::CredentialStatus::Invalid,
                scopes: None,
                expires_at: None,
            }),
            other => Err(other.into_app()),
        }
    }
}

/// The vault's probe for a Gemini key.
#[derive(Debug, Default)]
pub struct GeminiValidator {
    base_url: Option<String>,
}

impl GeminiValidator {
    /// Probes Google's API.
    pub fn new() -> Self {
        Self {
            base_url: std::env::var("ATLAS_GEMINI_BASE_URL").ok(),
        }
    }

    /// Probes a stand-in server (tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: Some(base_url.into()),
        }
    }
}

impl Validator for GeminiValidator {
    fn validate<'a>(
        &'a self,
        secret: &'a Secret<String>,
    ) -> std::pin::Pin<Box<dyn Future<Output = AppResult<ValidationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            GeminiClient::new(secret.clone(), self.base_url.as_deref())?
                .validate()
                .await
        })
    }
}

/// Price per image in USD for a model and size (paid tier, standard). From the research
/// dossier's pricing table; unknown models fall back to the flash-image price, so the cost
/// dashboard over- rather than under-reports.
pub fn image_price(model: &str, size: &str) -> f64 {
    let model = model.trim_end_matches("-preview");
    match (model, size) {
        ("gemini-3.1-flash-lite-image", _) => 0.0336,
        ("gemini-3-pro-image", "4K") => 0.24,
        ("gemini-3-pro-image", _) => 0.134,
        ("gemini-2.5-flash-image", _) => 0.039,
        (_, "512") => 0.045,
        (_, "2K") => 0.101,
        (_, "4K") => 0.151,
        _ => 0.067,
    }
}

/// Price of a text call in USD (flash-lite: $0.25/1M in, $1.50/1M out).
pub fn text_price(usage: Usage) -> f64 {
    #[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
    let (input, output) = (usage.input_tokens as f64, usage.output_tokens as f64);
    input * 0.25 / 1_000_000.0 + output * 1.5 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_key_is_a_400_and_is_still_recognised() {
        let body = r#"{"error":{"code":400,"message":"API key not valid.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID"}]}}"#;
        assert_eq!(classify(400, body), GeminiError::InvalidKey);
    }

    #[test]
    fn a_missing_key_has_no_details_and_is_still_recognised() {
        let body = r#"{"error":{"code":403,"message":"Method doesn't allow unregistered callers","status":"PERMISSION_DENIED"}}"#;
        assert_eq!(classify(403, body), GeminiError::InvalidKey);
    }

    #[test]
    fn a_malformed_request_is_not_mistaken_for_a_bad_key() {
        let body = r#"{"error":{"code":400,"message":"Invalid JSON payload","status":"INVALID_ARGUMENT","details":[]}}"#;
        assert!(matches!(classify(400, body), GeminiError::BadRequest(_)));
    }

    #[test]
    fn quota_billing_and_transient_are_distinct() {
        assert_eq!(
            classify(429, r#"{"error":{"status":"RESOURCE_EXHAUSTED"}}"#),
            GeminiError::QuotaExceeded
        );
        assert_eq!(
            classify(400, r#"{"error":{"status":"FAILED_PRECONDITION"}}"#),
            GeminiError::BillingRequired
        );
        assert!(matches!(
            classify(503, r#"{"error":{"status":"UNAVAILABLE"}}"#),
            GeminiError::Transient(_)
        ));
        assert!(matches!(
            classify(502, "<html>bad gateway</html>"),
            GeminiError::Transient(_)
        ));
    }

    #[test]
    fn images_are_found_among_interleaved_parts() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfake");
        let body = json!({
            "candidates": [{ "content": { "parts": [
                { "text": "Here you go." },
                { "inlineData": { "mimeType": "image/png", "data": png } },
            ] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 12, "candidatesTokenCount": 1120 },
        })
        .to_string();
        let generated = interpret(&body).unwrap();
        assert_eq!(generated.images.len(), 1);
        assert_eq!(generated.images[0].mime_type, "image/png");
        assert!(generated.images[0].bytes.starts_with(b"\x89PNG"));
        assert_eq!(generated.text, "Here you go.");
        assert_eq!(generated.usage.output_tokens, 1120);
    }

    #[test]
    fn a_200_with_no_image_is_a_block_not_a_panic() {
        let blocked_prompt = r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#;
        assert!(
            matches!(interpret(blocked_prompt), Err(GeminiError::Blocked(r)) if r.contains("SAFETY"))
        );
        let blocked_output = r#"{"candidates":[{"content":{"parts":[{"text":"no"}]},"finishReason":"IMAGE_SAFETY"}]}"#;
        assert!(
            matches!(interpret(blocked_output), Err(GeminiError::Blocked(r)) if r.contains("IMAGE_SAFETY"))
        );
    }

    #[test]
    fn prices_follow_the_table() {
        assert!((image_price("gemini-3.1-flash-image-preview", "1K") - 0.067).abs() < 1e-9);
        assert!((image_price("gemini-3.1-flash-lite-image", "1K") - 0.0336).abs() < 1e-9);
        assert!((image_price("gemini-3.1-flash-image", "2K") - 0.101).abs() < 1e-9);
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        assert!((text_price(usage) - 1.75).abs() < 1e-9);
    }
}
