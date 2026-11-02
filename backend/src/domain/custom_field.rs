//! Custom fields: a global registry of field definitions, a flat per-project layout, and typed
//! values on cards.
//!
//! # Three tables, three questions
//!
//! - `custom_fields` — *what is this field?* Global, unique by name (docs/adr/0003: a field
//!   must be one entity or cross-project queries break).
//! - `project_field_layout` — *does this project use it, for which card types, is it required,
//!   hidden, defaulted, and where does it sit?* Flat per-project rows; no schemes.
//! - `card_field_values` — *what does this card say?* One JSON value per (card, field).
//!
//! # Values are JSON, validated by type on every write
//!
//! [`validate_value`] is the single gate: it takes whatever JSON a client sent and either
//! returns the canonical stored form or a 422 naming the field. Nothing writes a value without
//! passing through it, so the shapes below are invariants of the table, not hopes:
//!
//! | type | stored as |
//! |---|---|
//! | `text` | string, ≤ 255 chars |
//! | `textarea` | string, ≤ 64 KiB (markdown source) |
//! | `number` | finite number |
//! | `date` | `"YYYY-MM-DD"` |
//! | `datetime` | RFC 3339 string, normalised to UTC |
//! | `select`, `radio` | one of the field's options |
//! | `multiselect` | array of distinct options |
//! | `checkbox` | boolean |
//! | `user` | a user id that exists |
//! | `multiuser` | array of distinct user ids that exist |
//! | `url` | `http(s)` URL |
//! | `labels` | array of distinct space-free labels |
//!
//! `null` always means "clear". An empty array or empty string also clears, rather than being
//! stored as a present-but-empty value that `IS EMPTY` would then disagree about.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::to_sql_timestamp;
use crate::db::Db;
use crate::domain::links;
use crate::error::{AppError, AppResult};

/// Longest field name.
pub const MAX_NAME: usize = 80;
/// Most options a choice field may have.
pub const MAX_OPTIONS: usize = 200;
/// Longest option.
pub const MAX_OPTION: usize = 100;

/// What kind of value a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// One line of text.
    Text,
    /// Markdown.
    Textarea,
    /// A number.
    Number,
    /// A calendar date.
    Date,
    /// An instant.
    Datetime,
    /// One of a list, as a dropdown.
    Select,
    /// Several of a list.
    Multiselect,
    /// Yes or no.
    Checkbox,
    /// One of a list, as radio buttons.
    Radio,
    /// One user.
    User,
    /// Several users.
    Multiuser,
    /// A web link.
    Url,
    /// Free-text labels.
    Labels,
}

impl FieldType {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Textarea => "textarea",
            Self::Number => "number",
            Self::Date => "date",
            Self::Datetime => "datetime",
            Self::Select => "select",
            Self::Multiselect => "multiselect",
            Self::Checkbox => "checkbox",
            Self::Radio => "radio",
            Self::User => "user",
            Self::Multiuser => "multiuser",
            Self::Url => "url",
            Self::Labels => "labels",
        }
    }

    /// Parses the stored spelling.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "text" => Self::Text,
            "textarea" => Self::Textarea,
            "number" => Self::Number,
            "date" => Self::Date,
            "datetime" => Self::Datetime,
            "select" => Self::Select,
            "multiselect" => Self::Multiselect,
            "checkbox" => Self::Checkbox,
            "radio" => Self::Radio,
            "user" => Self::User,
            "multiuser" => Self::Multiuser,
            "url" => Self::Url,
            "labels" => Self::Labels,
            _ => return None,
        })
    }

    /// Whether the field draws its values from `options`.
    pub fn has_options(self) -> bool {
        matches!(self, Self::Select | Self::Multiselect | Self::Radio)
    }
}

/// A row of `custom_fields` as stored.
#[derive(Debug, Clone, FromRow)]
struct FieldRow {
    id: String,
    name: String,
    field_type: String,
    description: Option<String>,
    options: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A global field definition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomField {
    /// UUID v7.
    pub id: String,
    /// Unique across the instance.
    pub name: String,
    /// What it holds.
    pub field_type: FieldType,
    /// What it is for.
    pub description: Option<String>,
    /// The choices, for select / multiselect / radio.
    pub options: Vec<String>,
    /// When it was defined.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<FieldRow> for CustomField {
    type Error = AppError;

    fn try_from(row: FieldRow) -> AppResult<Self> {
        let field_type = FieldType::parse(&row.field_type).ok_or_else(|| {
            AppError::internal(anyhow::anyhow!("unknown field type {:?}", row.field_type))
        })?;
        let options: Vec<String> =
            serde_json::from_str(&row.options).map_err(AppError::internal)?;
        Ok(Self {
            id: row.id,
            name: row.name,
            field_type,
            description: row.description,
            options,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// Checks a field name.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(AppError::Validation(format!(
            "A field name must be 1 to {MAX_NAME} printable characters."
        )));
    }
    if name.contains('"') {
        return Err(AppError::Validation(
            "A field name must not contain a double quote — queries quote it.".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

/// Checks a choice list: non-empty for a choice field, distinct, trimmed, bounded.
pub fn validate_options(field_type: FieldType, options: &[String]) -> AppResult<Vec<String>> {
    if !field_type.has_options() {
        return Ok(Vec::new());
    }
    let mut clean: Vec<String> = Vec::with_capacity(options.len());
    for option in options {
        let option = option.trim();
        if option.is_empty() || option.chars().count() > MAX_OPTION {
            return Err(AppError::Validation(format!(
                "Each option must be 1 to {MAX_OPTION} characters."
            )));
        }
        if clean.iter().any(|o| o.eq_ignore_ascii_case(option)) {
            return Err(AppError::Validation(format!("{option:?} is listed twice.")));
        }
        clean.push(option.to_owned());
    }
    if clean.is_empty() || clean.len() > MAX_OPTIONS {
        return Err(AppError::Validation(format!(
            "A {} field needs 1 to {MAX_OPTIONS} options.",
            field_type.as_str()
        )));
    }
    Ok(clean)
}

/// Every field definition, by name.
pub async fn list(db: &Db) -> AppResult<Vec<CustomField>> {
    sqlx::query_as::<_, FieldRow>(
        "SELECT id, name, field_type, description, options, created_at, updated_at \
         FROM custom_fields ORDER BY name",
    )
    .fetch_all(db.reader())
    .await?
    .into_iter()
    .map(CustomField::try_from)
    .collect()
}

/// Finds a field definition inside a transaction.
pub async fn find_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<CustomField>> {
    sqlx::query_as::<_, FieldRow>(
        "SELECT id, name, field_type, description, options, created_at, updated_at \
         FROM custom_fields WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .map(CustomField::try_from)
    .transpose()
}

fn name_taken(err: sqlx::Error, name: &str) -> AppError {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict(format!("A field called {name:?} already exists."))
        }
        _ => err.into(),
    }
}

/// Defines a field. `name` and `options` must already be validated.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    name: &str,
    field_type: FieldType,
    description: Option<&str>,
    options: &[String],
    now: DateTime<Utc>,
) -> AppResult<CustomField> {
    let id = Uuid::now_v7().to_string();
    let timestamp = to_sql_timestamp(now);
    sqlx::query(
        "INSERT INTO custom_fields (id, name, field_type, description, options, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(field_type.as_str())
    .bind(description)
    .bind(serde_json::to_string(options).map_err(AppError::internal)?)
    .bind(&timestamp)
    .bind(&timestamp)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_taken(err, name))?;
    find_tx(&mut *tx, &id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the field just defined is missing")))
}

/// Renames / redescribes a field, or replaces its options.
///
/// Removing an option that cards still hold is refused: the stored values would silently stop
/// being valid, and the next edit of those cards would fail on a value the user never touched.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    current: &CustomField,
    name: Option<&str>,
    description: Option<Option<String>>,
    options: Option<Vec<String>>,
    now: DateTime<Utc>,
) -> AppResult<CustomField> {
    let name = name.unwrap_or(&current.name);
    let description = description.unwrap_or_else(|| current.description.clone());
    let options = match options {
        Some(options) => {
            let options = validate_options(current.field_type, &options)?;
            for removed in current.options.iter().filter(|o| !options.contains(o)) {
                let in_use: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM card_field_values v WHERE v.field_id = ? \
                       AND (v.value = json_quote(?) \
                            OR EXISTS (SELECT 1 FROM json_each(v.value) e \
                                        WHERE json_valid(v.value) AND e.value = ?))",
                )
                .bind(&current.id)
                .bind(removed)
                .bind(removed)
                .fetch_one(&mut *tx)
                .await?;
                if in_use > 0 {
                    return Err(AppError::Conflict(format!(
                        "{in_use} card(s) still use the option {removed:?}. Change them first."
                    )));
                }
            }
            options
        }
        None => current.options.clone(),
    };
    sqlx::query(
        "UPDATE custom_fields SET name = ?, description = ?, options = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(name)
    .bind(&description)
    .bind(serde_json::to_string(&options).map_err(AppError::internal)?)
    .bind(to_sql_timestamp(now))
    .bind(&current.id)
    .execute(&mut *tx)
    .await
    .map_err(|err| name_taken(err, name))?;
    find_tx(&mut *tx, &current.id)
        .await?
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("the field just edited is missing")))
}

/// Deletes a field definition, every layout row and every value with it.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM custom_fields WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Value validation
// ---------------------------------------------------------------------------

/// The canonical stored form of `value` for `field`, or `None` for "clear".
// One arm per field type, each a few lines: the module doc's table is the spec and this is
// that table, row for row. Splitting it would hide the table across thirteen helpers.
///
/// User existence is checked by the caller-supplied `user_exists`, so this stays a pure
/// function of its inputs and can be unit-tested without a database.
#[allow(clippy::too_many_lines)]
pub fn validate_value(
    field: &CustomField,
    value: &Value,
    user_exists: &(dyn Fn(&str) -> bool + Sync),
) -> AppResult<Option<Value>> {
    let bad = |why: &str| AppError::Validation(format!("{}: {why}", field.name));
    if value.is_null() {
        return Ok(None);
    }
    let string = |v: &Value| v.as_str().map(str::trim).map(str::to_owned);
    Ok(Some(match field.field_type {
        FieldType::Text | FieldType::Textarea => {
            let text = string(value).ok_or_else(|| bad("must be text"))?;
            if text.is_empty() {
                return Ok(None);
            }
            let max = if field.field_type == FieldType::Text {
                255
            } else {
                64 * 1024
            };
            if text.chars().count() > max {
                return Err(bad(&format!("must be at most {max} characters")));
            }
            if field.field_type == FieldType::Text && text.contains('\n') {
                return Err(bad("must be a single line"));
            }
            Value::String(text)
        }
        FieldType::Number => {
            let number = value
                .as_f64()
                .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                .filter(|n| n.is_finite())
                .ok_or_else(|| bad("must be a number"))?;
            serde_json::Number::from_f64(number)
                .map(Value::Number)
                .ok_or_else(|| bad("must be a number"))?
        }
        FieldType::Date => {
            let text = string(value).ok_or_else(|| bad("must be a date"))?;
            if text.is_empty() {
                return Ok(None);
            }
            let date = NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .map_err(|_| bad("must be a date like 2026-03-01"))?;
            Value::String(date.to_string())
        }
        FieldType::Datetime => {
            let text = string(value).ok_or_else(|| bad("must be a date and time"))?;
            if text.is_empty() {
                return Ok(None);
            }
            let at = DateTime::parse_from_rfc3339(&text)
                .map_err(|_| bad("must be an RFC 3339 date-time"))?
                .with_timezone(&Utc);
            Value::String(to_sql_timestamp(at))
        }
        FieldType::Select | FieldType::Radio => {
            let choice = string(value).ok_or_else(|| bad("must be one of its options"))?;
            if choice.is_empty() {
                return Ok(None);
            }
            let canonical = field
                .options
                .iter()
                .find(|o| o.eq_ignore_ascii_case(&choice))
                .ok_or_else(|| bad(&format!("{choice:?} is not one of its options")))?;
            Value::String(canonical.clone())
        }
        FieldType::Multiselect => {
            let items = string_list(value).ok_or_else(|| bad("must be a list of options"))?;
            let mut out: Vec<Value> = Vec::new();
            for item in items {
                let canonical = field
                    .options
                    .iter()
                    .find(|o| o.eq_ignore_ascii_case(&item))
                    .ok_or_else(|| bad(&format!("{item:?} is not one of its options")))?;
                let canonical = Value::String(canonical.clone());
                if !out.contains(&canonical) {
                    out.push(canonical);
                }
            }
            if out.is_empty() {
                return Ok(None);
            }
            Value::Array(out)
        }
        FieldType::Checkbox => Value::Bool(
            value
                .as_bool()
                .ok_or_else(|| bad("must be true or false"))?,
        ),
        FieldType::User => {
            let id = string(value).ok_or_else(|| bad("must be a user id"))?;
            if id.is_empty() {
                return Ok(None);
            }
            if !user_exists(&id) {
                return Err(bad(&format!("{id:?} is not a user")));
            }
            Value::String(id)
        }
        FieldType::Multiuser => {
            let ids = string_list(value).ok_or_else(|| bad("must be a list of user ids"))?;
            let mut out: Vec<Value> = Vec::new();
            for id in ids {
                if !user_exists(&id) {
                    return Err(bad(&format!("{id:?} is not a user")));
                }
                let id = Value::String(id);
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            if out.is_empty() {
                return Ok(None);
            }
            Value::Array(out)
        }
        FieldType::Url => {
            let text = string(value).ok_or_else(|| bad("must be a URL"))?;
            if text.is_empty() {
                return Ok(None);
            }
            Value::String(links::validate_url(&text).map_err(|_| bad("must be an http(s) URL"))?)
        }
        FieldType::Labels => {
            let items = string_list(value).ok_or_else(|| bad("must be a list of labels"))?;
            let mut out: Vec<Value> = Vec::new();
            for item in items {
                if item.chars().any(char::is_whitespace) || item.chars().count() > 50 {
                    return Err(bad(&format!(
                        "{item:?} is not a label — labels have no spaces and are at most 50 characters"
                    )));
                }
                let item = Value::String(item);
                if !out.contains(&item) {
                    out.push(item);
                }
            }
            if out.is_empty() {
                return Ok(None);
            }
            Value::Array(out)
        }
    }))
}

/// A JSON array of strings (trimmed, empties dropped), or a single string as a one-item list.
fn string_list(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(s) => Some(
            Some(s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(|s| s.trim().to_owned()))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.into_iter().filter(|s| !s.is_empty()).collect()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// One row of a project's field layout, joined with its definition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LayoutEntry {
    /// The layout row.
    pub id: String,
    /// The field definition.
    pub field: CustomField,
    /// `None` = every card type.
    pub card_type_id: Option<String>,
    /// Must have a value.
    pub required: bool,
    /// Not shown on the card (but still queryable and settable).
    pub hidden: bool,
    /// Pre-filled for new cards.
    pub default_value: Option<Value>,
    /// Display order.
    pub position: i64,
}

#[derive(Debug, FromRow)]
struct LayoutRow {
    id: String,
    field_id: String,
    card_type_id: Option<String>,
    required: bool,
    hidden: bool,
    default_value: Option<String>,
    position: i64,
}

/// A project's field layout, in display order.
pub async fn layout(db: &Db, project_id: &str) -> AppResult<Vec<LayoutEntry>> {
    let rows = sqlx::query_as::<_, LayoutRow>(
        "SELECT id, field_id, card_type_id, required, hidden, default_value, position \
         FROM project_field_layout WHERE project_id = ? ORDER BY position, id",
    )
    .bind(project_id)
    .fetch_all(db.reader())
    .await?;
    let fields = list(db).await?;
    rows.into_iter()
        .filter_map(|row| {
            let field = fields.iter().find(|f| f.id == row.field_id)?.clone();
            Some((row, field))
        })
        .map(|(row, field)| {
            Ok(LayoutEntry {
                id: row.id,
                field,
                card_type_id: row.card_type_id,
                required: row.required,
                hidden: row.hidden,
                default_value: row
                    .default_value
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(AppError::internal)?,
                position: row.position,
            })
        })
        .collect()
}

/// One entry of a layout replacement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutInput {
    /// The field definition.
    pub field_id: String,
    /// `None` = every card type.
    #[serde(default)]
    pub card_type_id: Option<String>,
    /// Must have a value.
    #[serde(default)]
    pub required: bool,
    /// Not shown on the card.
    #[serde(default)]
    pub hidden: bool,
    /// Pre-filled for new cards. Validated against the field's type.
    #[serde(default)]
    pub default_value: Option<Value>,
}

/// Replaces a project's whole layout, in the order given.
pub async fn replace_layout(
    tx: &mut sqlx::SqliteConnection,
    project_id: &str,
    entries: &[LayoutInput],
    user_exists: &(dyn Fn(&str) -> bool + Sync),
) -> AppResult<()> {
    sqlx::query("DELETE FROM project_field_layout WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    for (position, entry) in entries.iter().enumerate() {
        let field = find_tx(&mut *tx, &entry.field_id).await?.ok_or_else(|| {
            AppError::Validation(format!("{:?} is not a custom field.", entry.field_id))
        })?;
        if let Some(type_id) = &entry.card_type_id {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT project_id FROM card_types WHERE id = ?")
                    .bind(type_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if owner.as_deref() != Some(project_id) {
                return Err(AppError::Validation(format!(
                    "{type_id:?} is not a card type of this project."
                )));
            }
        }
        let default = match &entry.default_value {
            Some(value) => validate_value(&field, value, user_exists)?,
            None => None,
        };
        sqlx::query(
            "INSERT INTO project_field_layout (id, project_id, field_id, card_type_id, required, \
             hidden, default_value, position) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(project_id)
        .bind(&field.id)
        .bind(&entry.card_type_id)
        .bind(entry.required)
        .bind(entry.hidden)
        .bind(default.map(|v| v.to_string()))
        .bind(i64::try_from(position).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await
        .map_err(|err| match &err {
            sqlx::Error::Database(db) if db.is_unique_violation() => AppError::Validation(format!(
                "{:?} is laid out twice for the same card type.",
                field.name
            )),
            _ => err.into(),
        })?;
    }
    Ok(())
}

/// The layout entries that apply to a card of `type_id`: a type-specific row wins over the
/// all-types row for the same field.
pub fn applicable(layout: &[LayoutEntry], type_id: &str) -> Vec<LayoutEntry> {
    let mut out: Vec<LayoutEntry> = Vec::new();
    for entry in layout {
        match entry.card_type_id.as_deref() {
            Some(t) if t != type_id => continue,
            _ => {}
        }
        if let Some(existing) = out.iter_mut().find(|e| e.field.id == entry.field.id) {
            if entry.card_type_id.is_some() {
                *existing = entry.clone();
            }
        } else {
            out.push(entry.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A card's custom field, with its value (or the layout default when unset).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardFieldValue {
    /// The layout entry this value is shown through.
    pub entry: LayoutEntry,
    /// The stored value; `None` = unset.
    pub value: Option<Value>,
}

/// Every stored value on a card, by field id.
pub async fn values(db: &Db, card_id: &str) -> AppResult<Vec<(String, Value)>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT field_id, value FROM card_field_values WHERE card_id = ?")
            .bind(card_id)
            .fetch_all(db.reader())
            .await?;
    rows.into_iter()
        .map(|(id, raw)| Ok((id, serde_json::from_str(&raw).map_err(AppError::internal)?)))
        .collect()
}

/// The stored value of one field on a card, inside a transaction.
pub async fn value_tx(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    field_id: &str,
) -> AppResult<Option<Value>> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value FROM card_field_values WHERE card_id = ? AND field_id = ?",
    )
    .bind(card_id)
    .bind(field_id)
    .fetch_optional(&mut *tx)
    .await?;
    raw.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(AppError::internal)
}

/// Sets (or clears, with `None`) one field's value on a card.
pub async fn set_value(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    field_id: &str,
    value: Option<&Value>,
) -> AppResult<()> {
    match value {
        Some(value) => {
            sqlx::query(
                "INSERT INTO card_field_values (card_id, field_id, value) VALUES (?, ?, ?) \
                 ON CONFLICT (card_id, field_id) DO UPDATE SET value = excluded.value",
            )
            .bind(card_id)
            .bind(field_id)
            .bind(value.to_string())
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM card_field_values WHERE card_id = ? AND field_id = ?")
                .bind(card_id)
                .bind(field_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    Ok(())
}

/// A value as a human reads it in the changelog.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Bool(true) => "Yes".to_owned(),
        Value::Bool(false) => "No".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(field_type: FieldType, options: &[&str]) -> CustomField {
        CustomField {
            id: "f".to_owned(),
            name: "Field".to_owned(),
            field_type,
            description: None,
            options: options.iter().map(|s| (*s).to_owned()).collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[allow(clippy::needless_pass_by_value)] // so every call site can pass `json!(..)` inline
    fn check(field_type: FieldType, options: &[&str], value: Value) -> AppResult<Option<Value>> {
        validate_value(&field(field_type, options), &value, &|id| {
            id == "u1" || id == "u2"
        })
    }

    #[test]
    fn every_type_has_a_stored_spelling_that_round_trips() {
        for t in [
            FieldType::Text,
            FieldType::Textarea,
            FieldType::Number,
            FieldType::Date,
            FieldType::Datetime,
            FieldType::Select,
            FieldType::Multiselect,
            FieldType::Checkbox,
            FieldType::Radio,
            FieldType::User,
            FieldType::Multiuser,
            FieldType::Url,
            FieldType::Labels,
        ] {
            assert_eq!(FieldType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn null_and_empty_always_clear() {
        for t in [
            FieldType::Text,
            FieldType::Select,
            FieldType::Labels,
            FieldType::User,
        ] {
            assert_eq!(check(t, &["a"], Value::Null).unwrap(), None);
        }
        assert_eq!(check(FieldType::Text, &[], json!("  ")).unwrap(), None);
        assert_eq!(
            check(FieldType::Multiselect, &["a"], json!([])).unwrap(),
            None
        );
    }

    #[test]
    fn scalars_are_typed_and_normalised() {
        assert_eq!(
            check(FieldType::Text, &[], json!(" Acme ")).unwrap(),
            Some(json!("Acme"))
        );
        assert!(check(FieldType::Text, &[], json!("a\nb")).is_err());
        assert!(check(FieldType::Text, &[], json!(5)).is_err());
        assert_eq!(
            check(FieldType::Number, &[], json!("42.5")).unwrap(),
            Some(json!(42.5))
        );
        assert!(check(FieldType::Number, &[], json!("lots")).is_err());
        assert_eq!(
            check(FieldType::Date, &[], json!("2026-03-01")).unwrap(),
            Some(json!("2026-03-01"))
        );
        assert!(check(FieldType::Date, &[], json!("01/03/2026")).is_err());
        assert!(
            check(FieldType::Datetime, &[], json!("2026-03-01T10:00:00+01:00"))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            check(FieldType::Checkbox, &[], json!(true)).unwrap(),
            Some(json!(true))
        );
        assert!(check(FieldType::Checkbox, &[], json!("yes")).is_err());
        assert!(check(FieldType::Url, &[], json!("javascript:alert(1)")).is_err());
    }

    #[test]
    fn choices_must_come_from_the_options_and_are_canonicalised() {
        assert_eq!(
            check(FieldType::Select, &["Remote", "Hybrid"], json!("remote")).unwrap(),
            Some(json!("Remote"))
        );
        assert!(check(FieldType::Radio, &["Remote"], json!("Office")).is_err());
        assert_eq!(
            check(FieldType::Multiselect, &["A", "B"], json!(["b", "A", "B"])).unwrap(),
            Some(json!(["B", "A"]))
        );
    }

    #[test]
    fn users_must_exist_and_labels_have_no_spaces() {
        assert_eq!(
            check(FieldType::User, &[], json!("u1")).unwrap(),
            Some(json!("u1"))
        );
        assert!(check(FieldType::User, &[], json!("ghost")).is_err());
        assert_eq!(
            check(FieldType::Multiuser, &[], json!(["u1", "u2", "u1"])).unwrap(),
            Some(json!(["u1", "u2"]))
        );
        assert!(check(FieldType::Labels, &[], json!(["two words"])).is_err());
        assert_eq!(
            check(FieldType::Labels, &[], json!(["remote", "remote"])).unwrap(),
            Some(json!(["remote"]))
        );
    }

    #[test]
    fn option_lists_are_required_distinct_and_only_for_choice_fields() {
        assert!(validate_options(FieldType::Select, &[]).is_err());
        assert!(validate_options(FieldType::Select, &["A".into(), "a".into()]).is_err());
        assert_eq!(
            validate_options(FieldType::Text, &["ignored".into()])
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn a_type_specific_layout_row_wins_over_the_all_types_row() {
        let f = field(FieldType::Text, &[]);
        let entry = |id: &str, type_id: Option<&str>, required: bool| LayoutEntry {
            id: id.to_owned(),
            field: f.clone(),
            card_type_id: type_id.map(str::to_owned),
            required,
            hidden: false,
            default_value: None,
            position: 0,
        };
        let layout = vec![entry("all", None, false), entry("bug", Some("bug"), true)];
        let for_bug = applicable(&layout, "bug");
        assert_eq!(for_bug.len(), 1);
        assert!(for_bug[0].required);
        let for_story = applicable(&layout, "story");
        assert_eq!(for_story.len(), 1);
        assert!(!for_story[0].required);
    }

    #[test]
    fn values_display_for_humans() {
        assert_eq!(display(&json!(["a", "b"])), "a, b");
        assert_eq!(display(&json!(true)), "Yes");
        assert_eq!(display(&json!(3.5)), "3.5");
    }
}
