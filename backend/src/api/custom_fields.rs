//! Custom fields: the global registry (`/custom-fields`), a project's layout
//! (`/projects/{key}/field-layout`), and a card's values (`/cards/{key}/fields`).
//!
//! Defining a field is instance administration — the registry is shared by every project, so
//! no project owner may rename a field out from under another project. Laying a field out on a
//! project is that project's owner's business; setting a value is card work.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::AppState;
use crate::api::serde_ext::double_option;
use crate::auth::extract::{RequireAdmin, RequireMember};
use crate::auth::{CurrentUser, now};
use crate::db::Db;
use crate::domain::card;
use crate::domain::custom_field::{
    self, CardFieldValue, CustomField, FieldType, LayoutEntry, LayoutInput,
};
use crate::domain::history;
use crate::domain::project;
use crate::error::{AppError, AppResult, Problem};

/// The body of `POST /custom-fields`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateFieldRequest {
    /// Unique across the instance.
    pub name: String,
    /// What it holds. Fixed once created — changing a field's type would invalidate every
    /// stored value at once.
    pub field_type: FieldType,
    /// What it is for.
    #[serde(default)]
    pub description: Option<String>,
    /// Choices, for select / multiselect / radio.
    #[serde(default)]
    pub options: Vec<String>,
}

/// The body of `PATCH /custom-fields/{id}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFieldRequest {
    /// A new name.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves it, `null` clears it.
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// The complete new option list. An option still in use cannot be removed.
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

/// The body of `PUT /projects/{key}/field-layout`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceLayoutRequest {
    /// The complete layout, in display order.
    pub entries: Vec<LayoutInput>,
}

/// The body of `PATCH /cards/{key}/fields`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetFieldsRequest {
    /// Field id → new value. `null` clears.
    #[schema(value_type = Object)]
    pub values: HashMap<String, Value>,
}

/// Every user id, for value validation. An instance has tens of users, not millions.
async fn user_ids(db: &Db) -> AppResult<HashSet<String>> {
    Ok(sqlx::query_scalar::<_, String>("SELECT id FROM users")
        .fetch_all(db.reader())
        .await?
        .into_iter()
        .collect())
}

fn clean_description(description: Option<&str>) -> AppResult<Option<String>> {
    let Some(text) = description.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > 1024 {
        return Err(AppError::Validation(
            "A field description must be at most 1024 characters.".to_owned(),
        ));
    }
    Ok(Some(text.to_owned()))
}

/// Every custom field definition.
#[utoipa::path(
    get,
    path = "/custom-fields",
    tag = "custom-fields",
    responses(
        (status = 200, description = "The global field registry, by name", body = Vec<CustomField>),
        (status = 401, description = "Not signed in", body = Problem),
    )
)]
async fn list_fields(
    State(state): State<AppState>,
    _current: CurrentUser,
) -> AppResult<Json<Vec<CustomField>>> {
    Ok(Json(custom_field::list(&state.db).await?))
}

/// Defines a field. Instance admins only — the registry is shared by every project.
#[utoipa::path(
    post,
    path = "/custom-fields",
    tag = "custom-fields",
    request_body = CreateFieldRequest,
    responses(
        (status = 201, description = "Defined", body = CustomField),
        (status = 403, description = "Not an admin", body = Problem),
        (status = 409, description = "The name is taken", body = Problem),
        (status = 422, description = "Invalid name or options", body = Problem),
    )
)]
async fn create_field(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(body): Json<CreateFieldRequest>,
) -> AppResult<(StatusCode, Json<CustomField>)> {
    let name = custom_field::validate_name(&body.name)?;
    let options = custom_field::validate_options(body.field_type, &body.options)?;
    let description = clean_description(body.description.as_deref())?;
    let mut tx = state.db.begin_write().await?;
    let field = custom_field::insert(
        &mut tx,
        &name,
        body.field_type,
        description.as_deref(),
        &options,
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(field)))
}

/// Renames a field, edits its description, or replaces its options.
#[utoipa::path(
    patch,
    path = "/custom-fields/{id}",
    tag = "custom-fields",
    params(("id" = String, Path, description = "The field")),
    request_body = UpdateFieldRequest,
    responses(
        (status = 200, description = "Edited", body = CustomField),
        (status = 404, description = "No such field", body = Problem),
        (status = 409, description = "Name taken, or an option still in use", body = Problem),
    )
)]
async fn update_field(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<String>,
    Json(body): Json<UpdateFieldRequest>,
) -> AppResult<Json<CustomField>> {
    let name = body
        .name
        .as_deref()
        .map(custom_field::validate_name)
        .transpose()?;
    let description = body
        .description
        .map(|d| clean_description(d.as_deref()))
        .transpose()?;
    let mut tx = state.db.begin_write().await?;
    let current = custom_field::find_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let updated = custom_field::update(
        &mut tx,
        &current,
        name.as_deref(),
        description,
        body.options,
        now(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(updated))
}

/// Deletes a field, its layout rows, and every card's value for it.
#[utoipa::path(
    delete,
    path = "/custom-fields/{id}",
    tag = "custom-fields",
    params(("id" = String, Path, description = "The field")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such field", body = Problem),
    )
)]
async fn delete_field(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let mut tx = state.db.begin_write().await?;
    custom_field::find_tx(&mut tx, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    custom_field::delete(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A project's field layout.
#[utoipa::path(
    get,
    path = "/projects/{key}/field-layout",
    tag = "custom-fields",
    params(("key" = String, Path, description = "The project key")),
    responses(
        (status = 200, description = "The layout, in display order", body = Vec<LayoutEntry>),
        (status = 404, description = "No such project", body = Problem),
    )
)]
async fn get_layout(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<LayoutEntry>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(custom_field::layout(&state.db, &project.id).await?))
}

/// Replaces a project's field layout.
#[utoipa::path(
    put,
    path = "/projects/{key}/field-layout",
    tag = "custom-fields",
    params(("key" = String, Path, description = "The project key")),
    request_body = ReplaceLayoutRequest,
    responses(
        (status = 200, description = "The new layout", body = Vec<LayoutEntry>),
        (status = 404, description = "No such project", body = Problem),
        (status = 422, description = "An unknown field, type, or invalid default", body = Problem),
    )
)]
async fn replace_layout(
    State(state): State<AppState>,
    _member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<ReplaceLayoutRequest>,
) -> AppResult<Json<Vec<LayoutEntry>>> {
    let project = project::find_by_key(&state.db, &key)
        .await?
        .ok_or(AppError::NotFound)?;
    let users = user_ids(&state.db).await?;
    let mut tx = state.db.begin_write().await?;
    custom_field::replace_layout(&mut tx, &project.id, &body.entries, &|id| {
        users.contains(id)
    })
    .await?;
    tx.commit().await?;
    Ok(Json(custom_field::layout(&state.db, &project.id).await?))
}

/// The custom fields that apply to a card, with their values.
async fn card_fields(db: &Db, target: &card::Card) -> AppResult<Vec<CardFieldValue>> {
    let layout = custom_field::layout(db, &target.project_id).await?;
    let applicable = custom_field::applicable(&layout, &target.type_id);
    let values = custom_field::values(db, &target.id).await?;
    Ok(applicable
        .into_iter()
        .map(|entry| {
            let value = values
                .iter()
                .find(|(id, _)| *id == entry.field.id)
                .map(|(_, v)| v.clone());
            CardFieldValue { entry, value }
        })
        .collect())
}

/// A card's custom fields: every field laid out for its type, with its value.
#[utoipa::path(
    get,
    path = "/cards/{key}/fields",
    tag = "custom-fields",
    params(("key" = String, Path, description = "The card key")),
    responses(
        (status = 200, description = "Fields and values", body = Vec<CardFieldValue>),
        (status = 404, description = "No such card", body = Problem),
    )
)]
async fn get_card_fields(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path(key): Path<String>,
) -> AppResult<Json<Vec<CardFieldValue>>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(card_fields(&state.db, &target).await?))
}

/// Sets custom field values on a card. Each change is written to the card's history.
#[utoipa::path(
    patch,
    path = "/cards/{key}/fields",
    tag = "custom-fields",
    params(("key" = String, Path, description = "The card key")),
    request_body = SetFieldsRequest,
    responses(
        (status = 200, description = "The card's fields now", body = Vec<CardFieldValue>),
        (status = 404, description = "No such card", body = Problem),
        (status = 422, description = "A field not laid out for this card, a bad value, or a required field cleared", body = Problem),
    )
)]
async fn set_card_fields(
    State(state): State<AppState>,
    member: RequireMember,
    Path(key): Path<String>,
    Json(body): Json<SetFieldsRequest>,
) -> AppResult<Json<Vec<CardFieldValue>>> {
    let target = card::find_by_key(&state.db, &key.to_ascii_uppercase())
        .await?
        .ok_or(AppError::NotFound)?;
    let layout = custom_field::layout(&state.db, &target.project_id).await?;
    let applicable = custom_field::applicable(&layout, &target.type_id);
    let users = user_ids(&state.db).await?;

    // Validate everything before writing anything: one bad value fails the whole request.
    let mut planned: Vec<(&LayoutEntry, Option<Value>)> = Vec::new();
    let mut ids: Vec<&String> = body.values.keys().collect();
    ids.sort();
    for field_id in ids {
        let entry = applicable
            .iter()
            .find(|e| e.field.id == *field_id)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{field_id:?} is not a field of this card's type in this project."
                ))
            })?;
        let value = custom_field::validate_value(&entry.field, &body.values[field_id], &|id| {
            users.contains(id)
        })?;
        if value.is_none() && entry.required {
            return Err(AppError::Validation(format!(
                "{} is required and cannot be cleared.",
                entry.field.name
            )));
        }
        planned.push((entry, value));
    }

    let at = now();
    let mut tx = state.db.begin_write().await?;
    for (entry, value) in planned {
        let before = custom_field::value_tx(&mut tx, &target.id, &entry.field.id).await?;
        if before == value {
            continue;
        }
        custom_field::set_value(&mut tx, &target.id, &entry.field.id, value.as_ref()).await?;
        history::record_custom(
            &mut tx,
            &target.id,
            Some(member.0.id()),
            &entry.field.id,
            (
                before.as_ref().map(Value::to_string),
                before.as_ref().map(custom_field::display),
            ),
            (
                value.as_ref().map(Value::to_string),
                value.as_ref().map(custom_field::display),
            ),
            at,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(card_fields(&state.db, &target).await?))
}

/// The custom field routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_fields, create_field))
        .routes(routes!(update_field, delete_field))
        .routes(routes!(get_layout, replace_layout))
        .routes(routes!(get_card_fields, set_card_fields))
}
