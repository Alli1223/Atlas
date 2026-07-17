//! Tags: free-text labels on cards, and the presets each template seeds.
//!
//! # Why this small module earns its place
//!
//! `TODO.md` marks Phase 4 ⭐ highest-value/lowest-cost, and the ratio is real.
//! A tag is a string on a join table. What it buys is the one axis of
//! organisation that costs the user nothing to invent: no admin screen, no
//! scheme, no migration. `hotfix` exists the moment someone types it.
//!
//! Almost all of the value is in migration 0004's two tables. This module is the
//! rules that a table cannot state:
//!
//! - [`validate_name`] — the no-whitespace rule.
//! - [`attach`] — a card may only carry its own project's tags, or a global one.
//! - [`merge`] — relink every card, then delete the source, without duplicating
//!   a `(card, tag)` pair and without orphaning a card.
//!
//! # Database access
//!
//! The runtime `sqlx::query_as::<_, T>("...")` API, as everything in
//! [`crate::domain`] does — see that module's note. Every SQL string here is a
//! `&'static str`, which satisfies sqlx 0.9's `SqlSafeStr` bound with no
//! `AssertSqlSafe`, so the absence of `AssertSqlSafe` is a real signal that no
//! SQL is built by formatting.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, FromRow, Sqlite, Type};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::db::Db;
use crate::domain::template::Template;
use crate::error::{AppError, AppResult};

/// Longest accepted tag name, in characters.
///
/// Shorter than [`crate::domain::config::MAX_NAME`] on purpose. A tag is
/// rendered as a chip on a card, and the chip is `max-width: 200px` with an
/// ellipsis — a 64-character tag is a chip that says `some-very-long-tag…` and
/// tells the reader nothing. The limit is a design constraint surfaced as a
/// validation rule rather than left to be discovered as a rendering bug.
pub const MAX_NAME: usize = 50;

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Why a tag colour could not be read.
#[derive(Debug, thiserror::Error)]
#[error("unknown tag colour {0:?}")]
pub struct TagColourError(String);

/// The palette a tag chip may be painted from.
///
/// # Why a closed enum and not a hex string
///
/// A chip is a *pair* of colours — a background and a text colour on top of it —
/// and the pair has to stay legible in light mode and dark mode. One hex cannot
/// be that pair. `#DCFFF1` is a readable green chip at noon and an eye-watering
/// one at midnight, and no amount of care at the point where the user picks it
/// changes that.
///
/// Each variant here names an ADS accent ramp. The frontend resolves it to
/// `--atlas-accent-{name}-bg` / `--atlas-accent-{name}-text`, which are already
/// defined for both themes in `frontend/src/styles/tokens.css` and were derived
/// from the verified ADS ramps by ADS's own rule (light: ramp-100 background,
/// ramp-800 text; dark: ramp-1000 background, ramp-300 text). Picking a name
/// picks a contrast-checked pair, in both themes, by construction.
///
/// This is the same mistake `docs/research/corrections.md` #9 records for board
/// cards — a literal colour where a token belonged, and dark mode broken by it.
///
/// The variants match `TAG_COLORS` in `frontend/src/components/ui/Tag.tsx`
/// exactly; a test in `tests/tags.rs` pins the list so the two cannot drift into
/// a chip that resolves to no CSS variable and renders invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TagColour {
    /// The default neutral chip.
    #[default]
    Standard,
    /// Neutral. Used for "no strong signal": `reference`, `dependencies`.
    Grey,
    /// Blue.
    Blue,
    /// Teal.
    Teal,
    /// Green. Conventionally the good ending.
    Green,
    /// Lime.
    Lime,
    /// Yellow. Conventionally "waiting on something".
    Yellow,
    /// Orange.
    Orange,
    /// Red. Conventionally trouble.
    Red,
    /// Magenta.
    Magenta,
    /// Purple.
    Purple,
}

impl TagColour {
    /// The colour's database, JSON and CSS spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Grey => "grey",
            Self::Blue => "blue",
            Self::Teal => "teal",
            Self::Green => "green",
            Self::Lime => "lime",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
            Self::Magenta => "magenta",
            Self::Purple => "purple",
        }
    }

    /// Every colour, for the picker.
    pub fn all() -> [Self; 11] {
        [
            Self::Standard,
            Self::Grey,
            Self::Blue,
            Self::Teal,
            Self::Green,
            Self::Lime,
            Self::Yellow,
            Self::Orange,
            Self::Red,
            Self::Magenta,
            Self::Purple,
        ]
    }
}

impl fmt::Display for TagColour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TagColour {
    type Err = TagColourError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| TagColourError(s.to_owned()))
    }
}

// The same sqlx shape as `StatusCategory` and `EstimationUnit`: stored as TEXT,
// validated on read. The CHECK constraint in migration 0004 and this Decode impl
// are two independent guards against a colour that resolves to no CSS variable.

impl Type<Sqlite> for TagColour {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for TagColour {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'q, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for TagColour {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// A tag row.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    /// UUID v7, as text.
    pub id: String,
    /// The owning project, or `None` for a global tag usable from anywhere.
    pub project_id: Option<String>,
    /// The label. Never contains whitespace — see [`validate_name`].
    pub name: String,
    /// An ADS accent name, or `None` for the neutral chip.
    pub colour: Option<TagColour>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

/// A tag, with how many of one project's cards carry it.
///
/// # What `usage_count` counts, and why it is not on [`Tag`]
///
/// It counts **live cards in the project being listed**. Both halves matter:
///
/// - *Live*: a soft-deleted card is in the trash, and a chip reading "3" that
///   opens an empty board is worse than no chip at all.
/// - *In this project*: a global tag has a different count in every project, so
///   the number is a property of the question, not of the tag. Putting it on
///   [`Tag`] would force every caller that only wants a name to pay for a
///   `GROUP BY`, and would make the field a lie in the one place it is read
///   without a project in hand.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagUsage {
    /// The tag itself.
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub tag: Tag,
    /// How many live cards of the listed project carry it. Zero is normal — a
    /// freshly seeded preset is used by nothing.
    pub usage_count: i64,
}

/// The columns of `tags`, in the order [`Tag`]'s `FromRow` expects.
macro_rules! tag_columns {
    () => {
        "id, project_id, name, colour, created_at"
    };
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Checks a tag name, returning it trimmed.
///
/// # The no-whitespace rule
///
/// Jira's rule, copied deliberately, and the reason is Phase 6. AQL has to parse
/// `tag = good-first-issue` and `tag IN (bug, hotfix)` out of a line the user
/// typed. A tag called `needs review` makes that line ambiguous — is it one tag,
/// or a tag and a stray word? — and the usual escape hatch, mandatory quoting,
/// makes the common case worse to type in order to permit a case nobody needs:
/// `needs-review` says everything `needs review` does.
///
/// So the ambiguity is refused at the only point where refusing it is cheap.
/// This is a rule about a *query language that does not exist yet*, which is
/// exactly why it has to be here now — the alternative is a migration that has
/// to rename other people's data.
///
/// # Why `char::is_whitespace` and not `== ' '`
///
/// A tab, a newline and a non-breaking space break the grammar in precisely the
/// same way a space does, and U+00A0 in particular is a character people paste
/// without ever seeing it. A check for the ASCII space alone would let it
/// through, and the tag would look correct in every UI and be unparseable by the
/// one thing the rule exists to protect.
///
/// # What is deliberately allowed
///
/// Everything else printable, including punctuation. `c++`, `i18n`, `.NET` and
/// `3d-print` are tags people mean. A future grammar that needs to say
/// `tag = "c++"` can quote it; the rule here is only the one that quoting cannot
/// rescue.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();

    if name.is_empty() {
        return Err(AppError::Validation(
            "A tag name must not be empty.".to_owned(),
        ));
    }

    // Whitespace is checked BEFORE control characters, and the order is the whole
    // difference between a useful message and a baffling one.
    //
    // A tab and a newline are *both* control characters and whitespace. Check
    // control first and `a<TAB>b` is refused as "contains control characters" —
    // true, unhelpful, and wrong about what the user did. They pasted something
    // out of a spreadsheet; to them that is a space, and "must not contain
    // spaces. Try \"a-b\"" is the sentence that gets them unstuck.
    //
    // The order costs nothing at the other end: NUL, ESC and DEL are not
    // whitespace, so they still fall through to the control branch and are still
    // named accurately.
    if name.chars().any(char::is_whitespace) {
        return Err(AppError::Validation(format!(
            "A tag name must not contain spaces. Try {:?}.",
            hyphenate(name)
        )));
    }

    if name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "A tag name must not contain control characters.".to_owned(),
        ));
    }

    if name.chars().count() > MAX_NAME {
        return Err(AppError::Validation(format!(
            "A tag name must be at most {MAX_NAME} characters long."
        )));
    }

    Ok(name.to_owned())
}

/// `needs review` → `needs-review`, for the rejection message.
///
/// A rule the user did not know about is an obstacle; a rule that shows them
/// what to type instead is a convention. The suggestion is not applied for them:
/// silently rewriting what someone typed is how you end up with tags nobody
/// meant to create.
fn hyphenate(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("-")
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Every tag a project can offer — its own and every global one — with usage
/// counts, by name.
///
/// # Why both `cards` filters are in the `ON` clause and not in `WHERE`
///
/// This is load-bearing rather than stylistic, and the reason is narrower than
/// the usual "WHERE turns a LEFT JOIN into an inner join" slogan — which is why
/// it is worth writing down.
///
/// For a tag with **no** cards at all, moving the filters to `WHERE` changes
/// nothing: the joined row is null-extended, and `NULL IS NULL` is true, so the
/// tag survives. That is exactly why a test over an unused tag passes either way
/// and proves nothing.
///
/// The break is a tag whose cards **all fail the filter** — every one of them
/// trashed, or (for a global tag) every one of them in a different project. Then
/// the joined rows are real and they fail the predicate, so `WHERE` drops them
/// and there is nothing left to null-extend: the tag disappears from the list
/// entirely rather than reading zero.
///
/// In product terms: trash your only `hotfix` card and `hotfix` vanishes from
/// the picker, so you cannot tag anything with it again. Silent, and reachable
/// only through the trash. `tests/tags.rs` pins both halves.
pub async fn list_for_project(db: &Db, project_id: &str) -> AppResult<Vec<TagUsage>> {
    Ok(sqlx::query_as::<_, TagUsage>(
        "SELECT t.id, t.project_id, t.name, t.colour, t.created_at, \
                COUNT(c.id) AS usage_count \
         FROM tags t \
         LEFT JOIN card_tags ct ON ct.tag_id = t.id \
         LEFT JOIN cards c \
                ON c.id = ct.card_id \
               AND c.project_id = ? \
               AND c.deleted_at IS NULL \
         WHERE t.project_id = ? OR t.project_id IS NULL \
         GROUP BY t.id \
         ORDER BY t.name",
    )
    .bind(project_id)
    .bind(project_id)
    .fetch_all(db.reader())
    .await?)
}

/// The tags on one card, by name.
pub async fn list_for_card(db: &Db, card_id: &str) -> AppResult<Vec<Tag>> {
    Ok(sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.project_id, t.name, t.colour, t.created_at \
         FROM tags t \
         JOIN card_tags ct ON ct.tag_id = t.id \
         WHERE ct.card_id = ? \
         ORDER BY t.name",
    )
    .bind(card_id)
    .fetch_all(db.reader())
    .await?)
}

/// The tags on one card, inside a transaction.
pub async fn list_for_card_tx(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
) -> AppResult<Vec<Tag>> {
    Ok(sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.project_id, t.name, t.colour, t.created_at \
         FROM tags t \
         JOIN card_tags ct ON ct.tag_id = t.id \
         WHERE ct.card_id = ? \
         ORDER BY t.name",
    )
    .bind(card_id)
    .fetch_all(&mut *tx)
    .await?)
}

/// Finds a tag by id.
pub async fn find_tx(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<Option<Tag>> {
    Ok(sqlx::query_as::<_, Tag>(concat!(
        "SELECT ",
        tag_columns!(),
        " FROM tags WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Whether a name is taken in a scope, ignoring one tag.
///
/// `except` is what makes a no-op rename (`bug` → `bug`, or a case change) work
/// instead of colliding with the row being renamed.
///
/// The `IS` operator rather than `=` on `project_id`: `NULL = NULL` is NULL, not
/// true, so `=` would report every global name as free and let the partial unique
/// index turn a 409 into a 500.
pub async fn name_taken_tx(
    tx: &mut sqlx::SqliteConnection,
    project_id: Option<&str>,
    name: &str,
    except: Option<&str>,
) -> AppResult<bool> {
    let taken: Option<String> = sqlx::query_scalar(
        "SELECT id FROM tags \
         WHERE project_id IS ? AND name = ? AND (? IS NULL OR id <> ?)",
    )
    .bind(project_id)
    .bind(name)
    .bind(except)
    .bind(except)
    .fetch_optional(&mut *tx)
    .await?;

    Ok(taken.is_some())
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Inserts a tag.
///
/// `project_id: None` creates a global tag.
pub async fn insert(
    tx: &mut sqlx::SqliteConnection,
    project_id: Option<&str>,
    name: &str,
    colour: Option<TagColour>,
    now: DateTime<Utc>,
) -> AppResult<Tag> {
    let id = Uuid::now_v7().to_string();

    sqlx::query(
        "INSERT INTO tags (id, project_id, name, colour, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(name)
    .bind(colour)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    Ok(Tag {
        id,
        project_id: project_id.map(ToOwned::to_owned),
        name: name.to_owned(),
        colour,
        created_at: now,
    })
}

/// What a [`update`] changes. `None` leaves a field alone.
#[derive(Debug, Default, Clone)]
pub struct TagPatch {
    /// The new name.
    pub name: Option<String>,
    /// The new colour. `Some(None)` clears it back to the neutral chip.
    pub colour: Option<Option<TagColour>>,
}

impl TagPatch {
    /// Whether this patch would change nothing.
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.colour.is_none()
    }
}

/// Renames and/or recolours a tag.
///
/// # Why a rename cannot orphan a card
///
/// It is not a property this function works to preserve — it is a property of
/// the schema. `card_tags.tag_id` points at `tags.id`, which nothing here
/// touches, so the name is not an identifier and renaming it is invisible to
/// every card carrying the tag. That is the whole reason tags are a join table
/// and not a comma-separated column: with `cards.labels = 'bug,urgent'`, a
/// rename is string surgery across every row and the failure mode is silent.
///
/// `tests/tags.rs` pins it anyway, because "obvious from the schema" is exactly
/// the kind of claim that stops being true when someone adds a denormalised
/// cache of tag names later.
pub async fn update(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    patch: &TagPatch,
    // `Some(None)` (clear) and `None` (leave) are different requests, and the
    // COALESCE trick that works for `name` cannot tell them apart — COALESCE
    // reads a NULL bind as "leave it", which is precisely the clear case. So the
    // colour gets its own statement, guarded by whether it was sent at all.
) -> AppResult<Tag> {
    if let Some(name) = &patch.name {
        sqlx::query("UPDATE tags SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    if let Some(colour) = &patch.colour {
        sqlx::query("UPDATE tags SET colour = ? WHERE id = ?")
            .bind(*colour)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    find_tx(&mut *tx, id).await?.ok_or(AppError::NotFound)
}

/// Deletes a tag, and with it every `(card, tag)` row that referenced it.
///
/// The cascade is `card_tags`' foreign key, not a second statement here. Which
/// means it cannot be forgotten, cannot half-run, and cannot be skipped by
/// anything that deletes a tag by some other path.
pub async fn delete(tx: &mut sqlx::SqliteConnection, id: &str) -> AppResult<()> {
    let deleted = sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    if deleted.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    Ok(())
}

/// Puts a tag on a card, if it is not already there.
///
/// Returns whether the card gained the tag; `false` means it already had it.
///
/// Re-tagging is a no-op rather than a 409 on purpose. "This card has `bug`" is
/// the caller's intent, and it is already true — a double-click on a chip in the
/// picker should not be an error, and the primary key means it cannot be a
/// duplicate row either way.
pub async fn attach(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    tag_id: &str,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO card_tags (card_id, tag_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(card_id)
    .bind(tag_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    Ok(inserted.rows_affected() > 0)
}

/// Takes a tag off a card.
///
/// Returns whether anything was removed.
pub async fn detach(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
    tag_id: &str,
) -> AppResult<bool> {
    let deleted = sqlx::query("DELETE FROM card_tags WHERE card_id = ? AND tag_id = ?")
        .bind(card_id)
        .bind(tag_id)
        .execute(&mut *tx)
        .await?;

    Ok(deleted.rows_affected() > 0)
}

/// Whether a tag is usable from a project: it is the project's own, or global.
///
/// The rule migration 0004 explicitly does not enforce — see its closing note.
/// Without it, `card_tags` would happily give a card in ATLAS a tag belonging to
/// a different project: the foreign key says "a tag", not "a tag this card is
/// allowed to have".
pub fn usable_from(tag: &Tag, project_id: &str) -> bool {
    match &tag.project_id {
        None => true,
        Some(owner) => owner == project_id,
    }
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

/// Merges `source` into `into`: every card carrying `source` ends up carrying
/// `into`, and `source` stops existing.
///
/// Returns how many cards gained `into` as a result — cards that already carried
/// both are not counted, because nothing changed for them.
///
/// # The two ways this goes wrong, and what stops each
///
/// **Orphaning.** Delete `source` first and the cascade takes its `card_tags`
/// rows with it, so the relink has nothing left to read and every card silently
/// loses the tag. Hence the order below: relink, *then* delete. Both statements
/// are in the caller's transaction, so a failure between them rolls back rather
/// than leaving half a merge.
///
/// **Duplicating.** A card carrying `bug` *and* `Bug` is the single most likely
/// reason anyone reaches for merge, and a plain `UPDATE card_tags SET tag_id`
/// would try to write a `(card, into)` row that already exists — a primary-key
/// violation, surfacing as a 500 on precisely the input the feature is for.
/// `INSERT OR IGNORE` from the source's rows makes that card a no-op instead.
///
/// # Why the scopes must match
///
/// Merging a global tag into a project's tag would take cards in *other*
/// projects — which the caller cannot see and did not ask about — and either
/// hand them a tag scoped to a project they are not in, or drop their tag
/// entirely. Neither is what "merge these two labels" means. So a merge stays
/// within one scope, and both directions of the cross-scope case are refused
/// rather than half-implemented.
pub async fn merge(
    tx: &mut sqlx::SqliteConnection,
    source_id: &str,
    into_id: &str,
    now: DateTime<Utc>,
) -> AppResult<u64> {
    if source_id == into_id {
        return Err(AppError::Validation(
            "A tag cannot be merged into itself.".to_owned(),
        ));
    }

    let source = find_tx(&mut *tx, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let into = find_tx(&mut *tx, into_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if source.project_id != into.project_id {
        return Err(AppError::Validation(
            "Both tags must belong to the same project, or both must be global. \
             Merging across projects would retag cards the caller cannot see."
                .to_owned(),
        ));
    }

    // Relink first. The reverse order would let the cascade delete the rows this
    // statement reads.
    //
    // `created_at` is the merged row's own, not `now`: the card has carried this
    // idea since it was tagged, and the tag it was filed under is a detail of
    // how the idea was spelled. A card that already had `into` keeps its
    // original row (OR IGNORE), which is the same answer from the other side.
    let relinked = sqlx::query(
        "INSERT OR IGNORE INTO card_tags (card_id, tag_id, created_at) \
         SELECT card_id, ?, COALESCE(created_at, ?) FROM card_tags WHERE tag_id = ?",
    )
    .bind(into_id)
    .bind(now)
    .bind(source_id)
    .execute(&mut *tx)
    .await?;

    // The cascade clears the source's own card_tags rows.
    delete(&mut *tx, source_id).await?;

    Ok(relinked.rows_affected())
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/// `(name, colour)`.
type TagSeed = (&'static str, TagColour);

/// The tags a template seeds, verbatim from `TODO.md` Phase 4.
///
/// # Why one list per template and not a union
///
/// The four sets in `TODO.md` are *alternatives*, not layers, and the data says
/// so: `blocked` appears in both Programming and General. Seeding a programming
/// project with both lists would insert `blocked` twice and be rejected by
/// `UNIQUE (project_id, name)` — the schema refusing an interpretation the prose
/// left open. So [`Template::Blank`] takes the General set, which is the only
/// one of the four that assumes nothing, and each other template takes its own.
///
/// # Why the colours are grouped, not gradiented
///
/// The brief asks for "a sensible colour scheme … (e.g. job-search stage tags
/// progress grey→blue→green)". Taken literally as fifteen distinct shades, each
/// tag would carry a colour nobody could decode: eleven accents cannot express
/// fifteen ranks, and a chip's colour is read at a glance or not at all.
///
/// So colour here encodes **kind**, and within the stage tags it encodes
/// progress exactly as asked: grey (sent, nothing back) → blue (in the process)
/// → green (offer). Two tags sharing a colour is a feature — it says they are
/// the same sort of thing.
pub fn presets(template: Template) -> &'static [TagSeed] {
    use TagColour::{Blue, Green, Grey, Lime, Magenta, Orange, Purple, Red, Teal, Yellow};

    match template {
        // Kind of work · state of flow · risk.
        Template::Programming => &[
            // What the work is.
            ("bug", Red),
            ("feature", Green),
            ("refactor", Blue),
            ("tech-debt", Orange),
            ("docs", Teal),
            ("testing", Purple),
            // The machinery around it — neutral, because it is never the point.
            ("ci", Grey),
            ("dependencies", Grey),
            // Risk. Red is reserved for "this can hurt".
            ("security", Red),
            ("breaking-change", Red),
            ("hotfix", Orange),
            ("performance", Yellow),
            // Flow.
            ("blocked", Red),
            ("needs-review", Yellow),
            // An invitation, and the only one — hence its own colour.
            ("good-first-issue", Lime),
        ],

        // The pipeline, coloured by the stage of the asset it belongs to:
        // geometry (blue) → surface (teal) → motion (purple) → light and render
        // (yellow/orange) → the verdict (magenta/green/red).
        Template::ThreeDModeling => &[
            ("modeling", Blue),
            ("sculpting", Blue),
            ("retopo", Blue),
            ("uv-unwrap", Teal),
            ("texturing", Teal),
            ("rigging", Purple),
            ("animation", Purple),
            ("lighting", Yellow),
            ("rendering", Orange),
            ("post-process", Orange),
            // Inputs and state.
            ("reference", Grey),
            ("wip", Yellow),
            // The verdict.
            ("client-review", Magenta),
            ("approved", Green),
            ("revision", Red),
        ],

        // The requested ramp, applied to the stage tags exactly: grey (sent,
        // silence) → blue (a human is reading it) → green (an offer).
        Template::JobSearch => &[
            ("applied", Grey),
            ("phone-screen", Blue),
            ("technical-interview", Blue),
            ("onsite", Blue),
            ("take-home", Blue),
            ("offer", Green),
            // Endings.
            ("rejected", Red),
            ("ghosted", Grey),
            // Things to do about it.
            ("follow-up", Yellow),
            ("referral", Purple),
            // What the job is, not where the application is. A separate band, so
            // `onsite` (a stage you reached) and `onsite-only` (a fact about the
            // role) never read as the same thing at a glance.
            ("remote", Teal),
            ("hybrid", Teal),
            ("onsite-only", Teal),
            ("contract", Magenta),
            ("permanent", Magenta),
        ],

        // The General set. Nothing here assumes a domain — which is why it is
        // what a blank project starts from.
        Template::Blank => &[
            ("urgent", Red),
            ("blocked", Red),
            ("waiting", Yellow),
            ("research", Purple),
            ("idea", Lime),
            ("question", Blue),
            ("admin", Grey),
        ],
    }
}

/// Seeds a project with its template's tag presets.
///
/// Called from [`crate::domain::template::apply`], inside the transaction that
/// creates the project — so a project either has its presets or does not exist.
pub async fn seed_presets(
    tx: &mut sqlx::SqliteConnection,
    template: Template,
    project_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    for (name, colour) in presets(template) {
        insert(&mut *tx, Some(project_id), name, Some(*colour), now).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_tag_name_with_a_space_is_rejected_and_told_what_to_type() {
        let err = validate_name("needs review").unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        // The suggestion is the point: a rule nobody was told about is an
        // obstacle, a rule that shows you the convention is a convention.
        assert!(
            err.to_string().contains("needs-review"),
            "expected a hyphenated suggestion, got: {err}"
        );
    }

    #[test]
    fn every_kind_of_whitespace_is_rejected_not_just_the_ascii_space() {
        // U+00A0 is the one that matters. It is pasted, not typed, it is
        // invisible in every UI, and it breaks the future query grammar exactly
        // as a plain space does. A `== ' '` check would let it straight through.
        //
        // Asserting the MESSAGE, not merely `is_err()`: a tab is both whitespace
        // and a control character, so which branch catches it is a real choice,
        // and `is_err()` would pass whichever one won. The frontend mirrors this
        // rule in `features/tags/name.ts`, and the two disagreeing is exactly the
        // drift these cases exist to catch.
        for name in [
            "a\u{00A0}b",
            "a\tb",
            "a\nb",
            "a\u{2007}b",
            "a b",
            "a\u{3000}b",
        ] {
            let err = validate_name(name).unwrap_err().to_string();
            assert!(
                err.contains("must not contain spaces"),
                "whitespace in {name:?} must be reported as a space, not as something \
                 the user has to decode; got: {err}"
            );
            assert!(
                err.contains("a-b"),
                "and the message must show what to type instead; got: {err}"
            );
        }
    }

    #[test]
    fn a_control_character_that_is_not_whitespace_is_named_accurately() {
        // The other side of the ordering: NUL, ESC and DEL are not whitespace,
        // so they must not be misreported as a space the user could hyphenate.
        for name in ["a\u{0}b", "a\u{1B}b", "a\u{7F}b"] {
            let err = validate_name(name).unwrap_err().to_string();
            assert!(
                err.contains("control characters"),
                "{name:?} should be reported as a control character; got: {err}"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_rejected() {
        // Leading/trailing space is a typing artefact, not an ambiguous name —
        // there is nothing to disambiguate once it is gone.
        assert_eq!(validate_name("  bug  ").unwrap(), "bug");
        assert_eq!(validate_name("\tbug\n").unwrap(), "bug");
    }

    #[test]
    fn punctuation_is_allowed_because_only_spaces_are_ambiguous() {
        // These are tags people mean. The no-spaces rule is not a general
        // sanitiser and must not grow into one.
        for name in ["c++", "i18n", ".NET", "3d-print", "v1.2.0", "@home", "a/b"] {
            assert_eq!(validate_name(name).unwrap(), name, "{name:?} must be legal");
        }
    }

    #[test]
    fn empty_control_and_overlong_names_are_rejected() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err(), "whitespace-only is empty");
        assert!(validate_name("a\u{0}b").is_err(), "control character");
        assert!(validate_name(&"a".repeat(MAX_NAME)).is_ok());
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
    }

    #[test]
    fn the_name_length_cap_counts_characters_not_bytes() {
        // A cap in bytes would let a user of a Latin-script language type 50
        // tags' worth of name and cut someone writing Japanese off at 16.
        let name = "\u{6f22}".repeat(MAX_NAME);
        assert_eq!(name.len(), MAX_NAME * 3, "3 bytes per character");
        assert!(validate_name(&name).is_ok());
    }

    #[test]
    fn tag_colours_round_trip_through_their_database_spelling() {
        for colour in TagColour::all() {
            assert_eq!(colour.as_str().parse::<TagColour>().unwrap(), colour);
            assert_eq!(
                serde_json::to_string(&colour).unwrap(),
                format!("\"{}\"", colour.as_str())
            );
        }

        // A colour with no CSS variable behind it renders an invisible chip, so
        // it is refused rather than defaulted.
        assert!("chartreuse".parse::<TagColour>().is_err());
        assert!("Blue".parse::<TagColour>().is_err(), "lowercase only");
        assert!("#4BADE8".parse::<TagColour>().is_err(), "not a hex");
    }

    #[test]
    fn every_preset_name_is_a_legal_tag_name() {
        // A seed that validate_name would reject is a project that cannot be
        // created — and it would fail on a user's machine, in a migration,
        // rather than here.
        for template in Template::all() {
            for (name, _) in presets(template) {
                assert_eq!(
                    validate_name(name).unwrap(),
                    *name,
                    "{template}: preset {name:?} is not a legal tag name"
                );
            }
        }
    }

    #[test]
    fn no_template_seeds_the_same_tag_twice() {
        // COLLATE NOCASE, so the collision the UNIQUE index would raise is
        // case-insensitive too — a list with `WIP` and `wip` is rejected by the
        // database, and should be rejected here first.
        for template in Template::all() {
            let mut seen = HashSet::new();
            for (name, _) in presets(template) {
                assert!(
                    seen.insert(name.to_lowercase()),
                    "{template}: {name:?} is seeded twice; UNIQUE (project_id, name) \
                     would reject the whole project"
                );
            }
        }
    }

    #[test]
    fn every_template_seeds_the_list_todo_md_documents() {
        // Verbatim from TODO.md Phase 4. These lists were requested by name; a
        // "sensible improvement" to one of them is a silent change to what the
        // product ships, so the test states them rather than deriving them.
        let expected: [(Template, &[&str]); 4] = [
            (
                Template::Programming,
                &[
                    "bug",
                    "feature",
                    "refactor",
                    "tech-debt",
                    "docs",
                    "testing",
                    "ci",
                    "security",
                    "performance",
                    "dependencies",
                    "breaking-change",
                    "good-first-issue",
                    "blocked",
                    "needs-review",
                    "hotfix",
                ],
            ),
            (
                Template::ThreeDModeling,
                &[
                    "modeling",
                    "sculpting",
                    "retopo",
                    "uv-unwrap",
                    "texturing",
                    "rigging",
                    "animation",
                    "lighting",
                    "rendering",
                    "post-process",
                    "reference",
                    "wip",
                    "client-review",
                    "approved",
                    "revision",
                ],
            ),
            (
                Template::JobSearch,
                &[
                    "applied",
                    "phone-screen",
                    "technical-interview",
                    "onsite",
                    "take-home",
                    "offer",
                    "rejected",
                    "ghosted",
                    "follow-up",
                    "referral",
                    "remote",
                    "hybrid",
                    "onsite-only",
                    "contract",
                    "permanent",
                ],
            ),
            (
                Template::Blank,
                &[
                    "urgent", "blocked", "waiting", "research", "idea", "question", "admin",
                ],
            ),
        ];

        for (template, want) in expected {
            let got: HashSet<&str> = presets(template).iter().map(|(n, _)| *n).collect();
            let want: HashSet<&str> = want.iter().copied().collect();

            let missing: Vec<_> = want.difference(&got).collect();
            let extra: Vec<_> = got.difference(&want).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{template}: missing {missing:?}, unexpected {extra:?}"
            );
        }
    }

    #[test]
    fn the_general_set_goes_to_the_blank_template_and_nowhere_else() {
        // The four TODO.md lists are alternatives, not layers, and `blocked`
        // proves it: it is in both Programming and General, so a union would
        // insert it twice and UNIQUE (project_id, name) would reject the
        // project. This test is the reason for the 1:1 mapping.
        let programming: HashSet<&str> = presets(Template::Programming)
            .iter()
            .map(|(n, _)| *n)
            .collect();
        let general: HashSet<&str> = presets(Template::Blank).iter().map(|(n, _)| *n).collect();

        assert!(
            programming.contains("blocked") && general.contains("blocked"),
            "the overlap that forces the sets to be alternatives"
        );
    }

    #[test]
    fn the_job_search_stage_tags_progress_grey_to_blue_to_green() {
        // The requested ramp, asserted rather than trusted: silence is grey,
        // being read by a human is blue, an offer is green.
        let by_name = |want: &str| match presets(Template::JobSearch)
            .iter()
            .find(|(name, _)| *name == want)
        {
            Some((_, colour)) => *colour,
            None => panic!("job-search has no {want:?} tag"),
        };

        assert_eq!(by_name("applied"), TagColour::Grey);
        for stage in ["phone-screen", "technical-interview", "onsite", "take-home"] {
            assert_eq!(by_name(stage), TagColour::Blue, "{stage} is mid-process");
        }
        assert_eq!(by_name("offer"), TagColour::Green);

        // And the two endings are not the same colour as the stages they follow.
        assert_eq!(by_name("rejected"), TagColour::Red);

        // `onsite` (a stage) and `onsite-only` (a fact about the role) are
        // different sorts of thing and must not read alike.
        assert_ne!(
            by_name("onsite"),
            by_name("onsite-only"),
            "a stage and a work-mode must be distinguishable at a glance"
        );
    }

    #[test]
    fn every_preset_colour_is_one_the_frontend_can_render() {
        // TagColour is closed, so this cannot fail by construction — but it is
        // the assertion that will fire if someone widens the enum without
        // widening tokens.css, and it costs nothing.
        let known: HashSet<&str> = TagColour::all().iter().map(|c| c.as_str()).collect();
        for template in Template::all() {
            for (name, colour) in presets(template) {
                assert!(
                    known.contains(colour.as_str()),
                    "{template}: {name:?} has an unrenderable colour"
                );
            }
        }
    }

    #[test]
    fn a_global_tag_is_usable_from_any_project_and_a_project_tag_is_not() {
        let global = Tag {
            id: "t1".into(),
            project_id: None,
            name: "urgent".into(),
            colour: None,
            created_at: Utc::now(),
        };
        let owned = Tag {
            project_id: Some("p1".into()),
            ..global.clone()
        };

        assert!(usable_from(&global, "p1"));
        assert!(usable_from(&global, "p2"), "global means everywhere");
        assert!(usable_from(&owned, "p1"));
        assert!(
            !usable_from(&owned, "p2"),
            "a card must not carry another project's tag"
        );
    }

    #[test]
    fn an_empty_patch_is_recognised_as_changing_nothing() {
        assert!(TagPatch::default().is_empty());
        assert!(
            !TagPatch {
                colour: Some(None),
                ..Default::default()
            }
            .is_empty(),
            "clearing the colour is a change, not an absence"
        );
    }
}
