//! AI generation (`TODO.md` Phase 14): project cover art and avatars, card covers, 3D
//! reference images, card-description drafting, and natural-language → AQL.
//!
//! Every generation is recorded in `generations` (what was asked, by whom, what it cost,
//! whether it was blocked) — that is both the "pick from grid" history and the cost
//! dashboard's Gemini half. Images are decoded from Gemini's base64 and stored under
//! `uploads/generated/{id}`, served with the same hardening as attachments.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Json, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::api::AppState;
use crate::auth::extract::RequireMember;
use crate::auth::{CurrentUser, now, to_sql_timestamp};
use crate::domain::project::{self, Project};
use crate::domain::{card, template::Template};
use crate::error::{AppError, AppResult, Problem};
use crate::integrations::gemini::{self, GeminiClient, GeminiError};
use crate::secrets::{self, CredentialStatus, Provider};
use crate::storage::{self, Area};

/// What an image is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ImagePurpose {
    /// A project's wide cover banner.
    ProjectCover,
    /// A project's square avatar.
    ProjectAvatar,
    /// A card's cover.
    CardCover,
    /// A reference image for 3D work, attached to the card.
    Reference,
}

impl ImagePurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProjectCover => "project_cover",
            Self::ProjectAvatar => "project_avatar",
            Self::CardCover => "card_cover",
            Self::Reference => "reference",
        }
    }

    fn default_aspect(self) -> &'static str {
        match self {
            Self::ProjectCover => "16:9",
            Self::ProjectAvatar => "1:1",
            Self::CardCover => "3:2",
            Self::Reference => "4:3",
        }
    }
}

/// The style presets.
pub const STYLES: &[(&str, &str)] = &[
    (
        "illustration",
        "a clean flat vector illustration with a limited, harmonious palette",
    ),
    (
        "3d",
        "a colourful, tactile 3D render with soft studio lighting",
    ),
    ("photo", "a natural photograph with shallow depth of field"),
    (
        "watercolour",
        "a loose watercolour painting on textured paper",
    ),
    ("pixel", "crisp pixel art"),
    (
        "minimal",
        "a minimal geometric composition with lots of negative space",
    ),
];

const ASPECTS: &[&str] = &[
    "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
];

/// The body of `POST /projects/{key}/generate/images`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateImagesRequest {
    /// What the image is for.
    pub purpose: ImagePurpose,
    /// What to draw. Default: built from the project (and card) name and type.
    #[serde(default)]
    pub prompt: Option<String>,
    /// One of the style presets.
    #[serde(default)]
    pub style: Option<String>,
    /// Overrides the purpose's default aspect ratio.
    #[serde(default)]
    pub aspect_ratio: Option<String>,
    /// How many options to generate (1–4). Each is a separate request and costs separately.
    #[serde(default)]
    pub count: Option<u8>,
    /// The card, for card covers and reference images.
    #[serde(default)]
    pub card_key: Option<String>,
}

/// One stored image.
#[derive(Debug, Clone, Serialize, ToSchema, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    /// Its id.
    pub id: String,
    /// Where to fetch it.
    #[sqlx(skip)]
    pub url: String,
    /// `image/png` or `image/jpeg`.
    pub mime_type: String,
    /// Bytes.
    pub size_bytes: i64,
}

/// One generation: the request, its outcome, its cost.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    /// Its id.
    pub id: String,
    /// `project_cover`, `card_cover`, `draft`, `query`, ….
    pub purpose: String,
    /// The prompt sent.
    pub prompt: String,
    /// `succeeded`, `blocked`, `failed`.
    pub status: String,
    /// Why it was blocked or failed.
    pub error: Option<String>,
    /// What it cost.
    pub cost_usd: Option<f64>,
    /// The images.
    pub images: Vec<GeneratedImage>,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// The body of `POST /generate/text`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateTextRequest {
    /// `draft` (a card description) or `query` (natural language → AQL).
    pub purpose: String,
    /// What to draft from, or the question to turn into a query.
    pub input: String,
    /// The card being drafted for (a draft uses its summary and project).
    #[serde(default)]
    pub card_key: Option<String>,
}

/// A text generation's result.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedText {
    /// The draft, or the AQL.
    pub text: String,
    /// For a query: whether Atlas's own parser accepts it.
    pub valid: Option<bool>,
    /// For a query that does not parse: why.
    pub error: Option<String>,
    /// What it cost.
    pub cost_usd: f64,
}

/// The body of `POST /projects/{key}/cover|avatar` and `POST /cards/{key}/cover`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UseImageRequest {
    /// A generated image of this project; `null` clears.
    pub image_id: Option<String>,
}

/// The first usable Gemini key in the vault, opened.
async fn client(state: &AppState) -> AppResult<GeminiClient> {
    let vault = state.vault.as_deref().ok_or_else(|| {
        AppError::Conflict(
            "The secrets vault is sealed (no ATLAS_MASTER_KEY), so no Gemini key can be used."
                .to_owned(),
        )
    })?;
    let credential = secrets::list(&state.db)
        .await?
        .into_iter()
        .filter(|c| {
            c.provider == Provider::Gemini
                && !matches!(
                    c.status,
                    CredentialStatus::Invalid | CredentialStatus::Expired
                )
        })
        .max_by_key(|c| (c.status == CredentialStatus::Valid, c.created_at))
        .ok_or_else(|| {
            AppError::Conflict("Add a Gemini API key in Settings → Integrations first.".to_owned())
        })?;
    let key = vault.open(&credential)?;
    GeminiClient::new(key, state.config.gemini_base_url.as_deref())
}

fn template_noun(template: &str) -> &'static str {
    match Template::all().into_iter().find(|t| t.as_str() == template) {
        Some(Template::Programming) => "a software project",
        Some(Template::ThreeDModeling) => "a 3D modelling and art pipeline",
        Some(Template::JobSearch) => "a job search",
        _ => "a project",
    }
}

fn build_prompt(
    purpose: ImagePurpose,
    project: &Project,
    card_summary: Option<&str>,
    style: &str,
    custom: Option<&str>,
) -> String {
    let subject = custom.map(str::trim).filter(|p| !p.is_empty()).map_or_else(
        || match (purpose, card_summary) {
            (ImagePurpose::Reference, Some(summary)) => format!("A clear reference image for a 3D artist modelling: {summary}. Neutral background, even lighting, the subject fully in frame."),
            (_, Some(summary)) => format!("Cover art for a task called \"{summary}\" in {}, \"{}\".", template_noun(&project.template), project.name),
            (ImagePurpose::ProjectAvatar, None) => format!("An icon representing {} called \"{}\". Simple, bold, readable at small sizes, centred.", template_noun(&project.template), project.name),
            _ => format!("Banner cover art for {} called \"{}\".", template_noun(&project.template), project.name),
        },
        str::to_owned,
    );
    format!("{subject} Style: {style}. No text, no letters, no watermarks.")
}

/// One generation attempt to record.
struct Attempt<'a> {
    project_id: &'a str,
    card_id: Option<&'a str>,
    purpose: &'a str,
    model: &'a str,
    prompt: &'a str,
    user: &'a str,
}

async fn record(
    state: &AppState,
    attempt: &Attempt<'_>,
    outcome: &Result<(gemini::Usage, f64), GeminiError>,
) -> AppResult<String> {
    let Attempt {
        project_id,
        card_id,
        purpose,
        model,
        prompt,
        user,
    } = *attempt;
    let id = Uuid::now_v7().to_string();
    let (status, error, usage, cost) = match outcome {
        Ok((usage, cost)) => ("succeeded", None, Some(*usage), Some(*cost)),
        Err(GeminiError::Blocked(reason)) => ("blocked", Some(reason.clone()), None, Some(0.0)),
        Err(err) => ("failed", Some(format!("{err:?}")), None, Some(0.0)),
    };
    let mut tx = state.db.begin_write().await?;
    sqlx::query(
        "INSERT INTO generations (id, provider, model, project_id, card_id, purpose, prompt, status, error, \
         input_tokens, output_tokens, cost_usd, created_by, created_at) \
         VALUES (?, 'gemini', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(model)
    .bind(project_id)
    .bind(card_id)
    .bind(purpose)
    .bind(prompt)
    .bind(status)
    .bind(error)
    .bind(usage.map(|u| u.input_tokens))
    .bind(usage.map(|u| u.output_tokens))
    .bind(cost)
    .bind(user)
    .bind(to_sql_timestamp(now()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn store_image(
    state: &AppState,
    generation_id: &str,
    image: &gemini::Image,
) -> AppResult<GeneratedImage> {
    // Trust the bytes, not the label: only a sniffed PNG/JPEG/WebP is stored.
    let mime = storage::sniff_image(&image.bytes).ok_or_else(|| {
        AppError::internal(anyhow::anyhow!(
            "Gemini returned something that is not an image"
        ))
    })?;
    let id = Uuid::now_v7().to_string();
    let path = storage::path_for(&state.config.data_dir, Area::Generated, &id, None)?;
    let stored = storage::write_bytes(&image.bytes, &path).await?;
    let size = i64::try_from(stored.size).unwrap_or(i64::MAX);
    let mut tx = state.db.begin_write().await?;
    sqlx::query("INSERT INTO generated_images (id, generation_id, mime_type, size_bytes, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(generation_id)
        .bind(mime)
        .bind(size)
        .bind(to_sql_timestamp(now()))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(GeneratedImage {
        url: format!("/api/v1/generated-images/{id}"),
        id,
        mime_type: mime.to_owned(),
        size_bytes: size,
    })
}

async fn generation(state: &AppState, id: &str) -> AppResult<Generation> {
    let row: (String, String, String, String, Option<String>, Option<f64>, DateTime<Utc>) = sqlx::query_as(
        "SELECT id, purpose, prompt, status, error, cost_usd, created_at FROM generations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(state.db.reader())
    .await?;
    let images = sqlx::query_as::<_, GeneratedImage>("SELECT id, mime_type, size_bytes FROM generated_images WHERE generation_id = ? ORDER BY created_at, id")
        .bind(id)
        .fetch_all(state.db.reader())
        .await?
        .into_iter()
        .map(|mut i| {
            i.url = format!("/api/v1/generated-images/{}", i.id);
            i
        })
        .collect();
    Ok(Generation {
        id: row.0,
        purpose: row.1,
        prompt: row.2,
        status: row.3,
        error: row.4,
        cost_usd: row.5,
        images,
        created_at: row.6,
    })
}

/// Checks an image request's options: `(style text, aspect ratio, count)`.
fn check_options(body: &GenerateImagesRequest) -> AppResult<(&'static str, &str, u8)> {
    let style_name = body.style.as_deref().unwrap_or("illustration");
    let style = STYLES
        .iter()
        .find(|(name, _)| *name == style_name)
        .map(|(_, s)| *s)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "{style_name:?} is not a style. Use one of: {}.",
                STYLES
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    let aspect = body
        .aspect_ratio
        .as_deref()
        .unwrap_or_else(|| body.purpose.default_aspect());
    if !ASPECTS.contains(&aspect) {
        return Err(AppError::Validation(format!(
            "{aspect:?} is not a supported aspect ratio."
        )));
    }
    let count = body.count.unwrap_or(1);
    if !(1..=4).contains(&count) {
        return Err(AppError::Validation(
            "Generate 1 to 4 options at a time.".to_owned(),
        ));
    }
    if body
        .prompt
        .as_ref()
        .is_some_and(|p| p.chars().count() > 2000)
    {
        return Err(AppError::Validation(
            "A prompt must be at most 2000 characters.".to_owned(),
        ));
    }
    Ok((style, aspect, count))
}

/// Generates 1–4 image options. Each option is its own Gemini request.
#[utoipa::path(post, path = "/projects/{key}/generate/images", tag = "generate",
    params(("key" = String, Path, description = "The project key")),
    request_body = GenerateImagesRequest,
    responses((status = 201, description = "The generation and its images (blocked/failed ones are recorded too)", body = Generation),
              (status = 409, description = "No usable Gemini key, or Gemini refused for quota/billing", body = Problem),
              (status = 422, description = "A bad option, or the safety filter blocked it", body = Problem)))]
async fn generate_images(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<GenerateImagesRequest>,
) -> AppResult<(StatusCode, Json<Generation>)> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let (style, aspect, count) = check_options(&body)?;
    let target = match (&body.card_key, body.purpose) {
        (Some(key), _) => Some(
            card::find_by_key(&state.db, &key.to_ascii_uppercase())
                .await?
                .filter(|c| c.project_id == project.id && !c.is_deleted())
                .ok_or(AppError::NotFound)?,
        ),
        (None, ImagePurpose::CardCover | ImagePurpose::Reference) => {
            return Err(AppError::Validation(
                "Card covers and references need a cardKey.".to_owned(),
            ));
        }
        (None, _) => None,
    };
    let prompt = build_prompt(
        body.purpose,
        &project,
        target.as_ref().map(|c| c.summary.as_str()),
        style,
        body.prompt.as_deref(),
    );
    let gemini = client(&state).await?;
    let model = state.config.gemini_image_model.clone();
    let size = "1K";

    let attempts = futures_util::future::join_all(
        (0..count).map(|_| gemini.generate_image(&model, &prompt, aspect, size)),
    )
    .await;
    let mut images = Vec::new();
    let mut usage = gemini::Usage::default();
    let mut first_error = None;
    for attempt in attempts {
        match attempt {
            Ok(generated) => {
                usage.input_tokens += generated.usage.input_tokens;
                usage.output_tokens += generated.usage.output_tokens;
                images.extend(generated.images);
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    #[allow(clippy::cast_precision_loss)] // at most four images
    let cost = gemini::image_price(&model, size) * images.len() as f64;
    let outcome = if images.is_empty() {
        Err(first_error.unwrap_or(GeminiError::Blocked("no image".to_owned())))
    } else {
        Ok((usage, cost))
    };
    let id = record(
        &state,
        &Attempt {
            project_id: &project.id,
            card_id: target.as_ref().map(|c| c.id.as_str()),
            purpose: body.purpose.as_str(),
            model: &model,
            prompt: &prompt,
            user: member.0.id(),
        },
        &outcome,
    )
    .await?;
    if let Err(err) = outcome {
        // Recorded (so the history shows the attempt), then surfaced in words a user can act on.
        return Err(err.into_app());
    }
    for image in &images {
        let stored = store_image(&state, &id, image).await?;
        // A reference image is useful *on the card*: attach it, versioned like any upload.
        if body.purpose == ImagePurpose::Reference
            && let Some(card) = &target
        {
            attach_reference(&state, card, &stored, member.0.id()).await?;
        }
    }
    Ok((StatusCode::CREATED, Json(generation(&state, &id).await?)))
}

async fn attach_reference(
    state: &AppState,
    target: &card::Card,
    image: &GeneratedImage,
    user: &str,
) -> AppResult<()> {
    let source = storage::path_for(&state.config.data_dir, Area::Generated, &image.id, None)?;
    let bytes = tokio::fs::read(&source).await.map_err(AppError::internal)?;
    let ext = if image.mime_type == "image/jpeg" {
        "jpg"
    } else {
        "png"
    };
    let filename = format!(
        "reference-{}.{ext}",
        &image.id[image.id.len().saturating_sub(8)..]
    );
    let slot = {
        let mut tx = state.db.begin_write().await?;
        let slot = crate::domain::attachment::reserve(&mut tx, &target.id, &filename).await?;
        tx.commit().await?;
        slot
    };
    let path = storage::path_for(
        &state.config.data_dir,
        Area::Attachments,
        &slot.attachment_id,
        Some(slot.version),
    )?;
    let stored = storage::write_bytes(&bytes, &path).await?;
    let mut tx = state.db.begin_write().await?;
    crate::domain::attachment::record(
        &mut tx,
        &slot,
        &crate::domain::attachment::Upload {
            card_id: &target.id,
            filename: &filename,
            content_type: &image.mime_type,
            size_bytes: i64::try_from(stored.size).unwrap_or(i64::MAX),
            sha256: &stored.sha256,
            uploaded_by: Some(user),
        },
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A project's generation history, newest first.
#[utoipa::path(get, path = "/projects/{key}/generations", tag = "generate",
    params(("key" = String, Path, description = "The project key")),
    responses((status = 200, description = "Generations, newest first", body = Vec<Generation>)))]
async fn list_generations(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<Generation>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM generations WHERE project_id = ? ORDER BY created_at DESC LIMIT 50",
    )
    .bind(&project.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::new();
    for id in ids {
        out.push(generation(&state, &id).await?);
    }
    Ok(Json(out))
}

/// One generated image.
#[utoipa::path(get, path = "/generated-images/{id}", tag = "generate",
    params(("id" = String, Path, description = "The image")),
    responses((status = 200, description = "The image", content_type = "image/png"),
              (status = 404, description = "No such image", body = Problem)))]
async fn get_image(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(id): Path<String>,
) -> AppResult<Response> {
    let mime: String = sqlx::query_scalar("SELECT mime_type FROM generated_images WHERE id = ?")
        .bind(&id)
        .fetch_optional(state.db.reader())
        .await?
        .ok_or(AppError::NotFound)?;
    let path = storage::path_for(&state.config.data_dir, Area::Generated, &id, None)?;
    crate::api::attachments::serve_file(&path, "image", &mime, storage::is_inline_safe(&mime)).await
}

async fn image_of_project(state: &AppState, image_id: &str, project_id: &str) -> AppResult<()> {
    let owner: Option<Option<String>> = sqlx::query_scalar(
        "SELECT g.project_id FROM generated_images i JOIN generations g ON g.id = i.generation_id WHERE i.id = ?",
    )
    .bind(image_id)
    .fetch_optional(state.db.reader())
    .await?;
    if owner.flatten().as_deref() == Some(project_id) {
        Ok(())
    } else {
        Err(AppError::Validation(
            "That image was not generated for this project.".to_owned(),
        ))
    }
}

async fn set_project_image(
    state: &AppState,
    key: &str,
    body: &UseImageRequest,
    column: &'static str,
) -> AppResult<Json<Value>> {
    let project = project::find_by_key(&state.db, key)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(id) = &body.image_id {
        image_of_project(state, id, &project.id).await?;
    }
    let url = body
        .image_id
        .as_ref()
        .map(|id| format!("/api/v1/generated-images/{id}"));
    let sql = if column == "cover" {
        "UPDATE projects SET cover_image_url = ?, updated_at = ? WHERE id = ?"
    } else {
        "UPDATE projects SET avatar_url = ?, updated_at = ? WHERE id = ?"
    };
    let mut tx = state.db.begin_write().await?;
    sqlx::query(sql)
        .bind(&url)
        .bind(to_sql_timestamp(now()))
        .bind(&project.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "url": url })))
}

/// Sets (or clears) the project's cover from one of its generated images.
#[utoipa::path(post, path = "/projects/{key}/cover", tag = "generate",
    params(("key" = String, Path, description = "The project key")), request_body = UseImageRequest,
    responses((status = 200, description = "The cover URL", body = Object)))]
async fn set_project_cover(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<UseImageRequest>,
) -> AppResult<Json<Value>> {
    set_project_image(&state, &key, &body, "cover").await
}

/// Sets (or clears) the project's avatar from one of its generated images.
#[utoipa::path(post, path = "/projects/{key}/avatar", tag = "generate",
    params(("key" = String, Path, description = "The project key")), request_body = UseImageRequest,
    responses((status = 200, description = "The avatar URL", body = Object)))]
async fn set_project_avatar(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<UseImageRequest>,
) -> AppResult<Json<Value>> {
    set_project_image(&state, &key, &body, "avatar").await
}

/// Sets (or clears) a card's cover from one of its project's generated images.
#[utoipa::path(post, path = "/cards/{key}/cover", tag = "generate",
    params(("key" = String, Path, description = "The card key")), request_body = UseImageRequest,
    responses((status = 200, description = "The cover URL", body = Object)))]
async fn set_card_cover(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<UseImageRequest>,
) -> AppResult<Json<Value>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = state.db.begin_write().await?;
    match &body.image_id {
        Some(id) => {
            image_of_project(&state, id, &target.project_id).await?;
            sqlx::query("INSERT INTO card_covers (card_id, image_id, set_at) VALUES (?, ?, ?) ON CONFLICT (card_id) DO UPDATE SET image_id = excluded.image_id, set_at = excluded.set_at")
                .bind(&target.id).bind(id).bind(to_sql_timestamp(now())).execute(&mut *tx).await?;
        }
        None => {
            sqlx::query("DELETE FROM card_covers WHERE card_id = ?")
                .bind(&target.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({ "url": body.image_id.as_ref().map(|id| format!("/api/v1/generated-images/{id}")) }),
    ))
}

/// A card's cover, if it has one.
#[utoipa::path(get, path = "/cards/{key}/cover", tag = "generate",
    params(("key" = String, Path, description = "The card key")),
    responses((status = 200, description = "The cover URL, or null", body = Object)))]
async fn get_card_cover(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Value>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let id: Option<String> =
        sqlx::query_scalar("SELECT image_id FROM card_covers WHERE card_id = ?")
            .bind(&target.id)
            .fetch_optional(state.db.reader())
            .await?;
    Ok(Json(
        json!({ "url": id.map(|id| format!("/api/v1/generated-images/{id}")) }),
    ))
}

fn query_prompt(question: &str) -> String {
    format!(
        "You translate a question about work items into Atlas Query Language (AQL), a JQL-like language.\n\
         Fields: project, type, status, statusCategory (todo, in_progress, done), priority, assignee, reporter, creator, \
         resolution, parent, created, updated, due, resolved, started, summary, description, text, labels, key, estimate, \
         milestone, component.\n\
         Operators: = != > >= < <= IN \"NOT IN\" ~ !~ IS \"IS NOT\" WAS CHANGED, with EMPTY; keywords AND OR NOT ORDER BY ASC DESC.\n\
         Functions: currentUser(), now(), startOfWeek(-1w), endOfMonth(), customField(\"Name\", \"value\").\n\
         Relative dates look like -7d, -2w, +1d. Text fields use ~. Quote values with spaces.\n\
         Examples: \"my open bugs\" → type = Bug AND assignee = currentUser() AND statusCategory != done\n\
         \"applications with no reply for two weeks\" → status = Applied AND updated <= -2w\n\
         Return only JSON {{\"aql\": \"...\"}}.\n\nQuestion: {question}"
    )
}

/// Drafts a card description, or turns a question into AQL.
#[utoipa::path(post, path = "/generate/text", tag = "generate",
    request_body = GenerateTextRequest,
    responses((status = 200, description = "The text, and for a query whether it parses", body = GeneratedText),
              (status = 409, description = "No usable Gemini key", body = Problem)))]
async fn generate_text(
    State(state): State<AppState>,
    member: RequireMember,
    Json(body): Json<GenerateTextRequest>,
) -> AppResult<Json<GeneratedText>> {
    let input = body.input.trim();
    if input.is_empty() || input.chars().count() > 4000 {
        return Err(AppError::Validation(
            "Give 1 to 4000 characters to work from.".to_owned(),
        ));
    }
    let target = match &body.card_key {
        Some(key) => {
            let found = card::find_by_key(&state.db, &key.to_ascii_uppercase())
                .await?
                .ok_or(AppError::NotFound)?;
            // The body named the card, which the gate cannot see: check access here.
            let project = project::find_by_id(&state.db, &found.project_id)
                .await?
                .ok_or(AppError::NotFound)?;
            crate::domain::member::require(
                &state.db,
                &project,
                &member.0.user,
                crate::domain::member::ProjectRole::Member,
            )
            .await?;
            Some((found, project))
        }
        None => None,
    };
    let gemini = client(&state).await?;
    let model = state.config.gemini_text_model.clone();
    let (prompt, schema) = match body.purpose.as_str() {
        "draft" => {
            let context = target.as_ref().map_or_else(String::new, |(c, p)| {
                format!(
                    "The card is \"{}\" in {} \"{}\".\n",
                    c.summary,
                    template_noun(&p.template),
                    p.name
                )
            });
            (
                format!(
                    "Write a concise markdown description for a work card. {context}Include a short goal and a checklist of acceptance criteria as `- [ ]` items. Notes from the user:\n{input}"
                ),
                None,
            )
        }
        "query" => (
            query_prompt(input),
            Some(
                json!({ "type": "object", "properties": { "aql": { "type": "string" } }, "required": ["aql"] }),
            ),
        ),
        other => {
            return Err(AppError::Validation(format!(
                "{other:?} is not a text purpose; use draft or query."
            )));
        }
    };
    let result = gemini.generate_text(&model, &prompt, schema.as_ref()).await;
    let project_id = target.as_ref().map(|(_, p)| p.id.clone());
    let outcome = result
        .as_ref()
        .map(|g| (g.usage, gemini::text_price(g.usage)))
        .map_err(Clone::clone);
    if let Some(project_id) = &project_id {
        record(
            &state,
            &Attempt {
                project_id,
                card_id: target.as_ref().map(|(c, _)| c.id.as_str()),
                purpose: &body.purpose,
                model: &model,
                prompt: input,
                user: member.0.id(),
            },
            &outcome,
        )
        .await?;
    }
    let generated = result.map_err(GeminiError::into_app)?;
    let cost = gemini::text_price(generated.usage);
    if body.purpose == "query" {
        let aql = serde_json::from_str::<Value>(&generated.text)
            .ok()
            .and_then(|v| v["aql"].as_str().map(str::to_owned))
            .unwrap_or_else(|| generated.text.trim().to_owned());
        let parsed = crate::aql::parse(&aql);
        return Ok(Json(GeneratedText {
            valid: Some(parsed.is_ok()),
            error: parsed.err().map(|e| e.to_string()),
            text: aql,
            cost_usd: cost,
        }));
    }
    Ok(Json(GeneratedText {
        text: generated.text,
        valid: None,
        error: None,
        cost_usd: cost,
    }))
}

/// The generation routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(generate_images))
        .routes(routes!(list_generations))
        .routes(routes!(get_image))
        .routes(routes!(set_project_cover))
        .routes(routes!(set_project_avatar))
        .routes(routes!(get_card_cover, set_card_cover))
        .routes(routes!(generate_text))
}
