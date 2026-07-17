//! Walking the card tree: ancestors, descendants, depth, and the two guards
//! that keep a uniform `parent_id` from eating itself.
//!
//! # Why guards are mandatory here rather than nice to have
//!
//! ADR 0002 chose one uniform parent pointer at every level, which is what makes
//! nested boards and Epic→Story→Sub-task the same mechanism. The bill for that
//! choice is paid here: **a uniform parent pointer is a graph, and graphs have
//! cycles**. Jira never has to think about this because its three levels are
//! hardcoded and a sub-task is structurally forbidden from having children.
//!
//! Atlas has no such luck, and the board hits this path constantly — dragging a
//! card into another card *is* a reparent. So:
//!
//! - [`check_reparent`] refuses any move that would make a card its own
//!   ancestor, and
//! - [`MAX_DEPTH`] caps the tree, so a bug surfaces as a friendly 409 rather
//!   than a stack overflow or a query that never returns.
//!
//! # Reading the tree with corrupt data in it
//!
//! Every recursive query below carries a `distance < WALK_LIMIT` guard. If a
//! cycle ever *does* get into the table — an operator with a SQL prompt, a bug
//! in a future bulk operation — an unguarded `WITH RECURSIVE ... UNION ALL`
//! spins forever and takes the request, the connection and eventually the writer
//! pool with it. The guard turns that into a bounded read and a loud error.

use sqlx::FromRow;

use crate::domain::card::Card;
use crate::error::{AppError, AppResult};

/// The deepest a card tree may go, counting the root as 1.
///
/// Five is a guard against pathology, not a considered maximum (ADR 0002 says so
/// explicitly). It exists so that a cycle bug surfaces as a friendly error
/// instead of a stack overflow, and so roll-up queries have a bounded cost. It
/// comfortably covers every seeded template: the deepest is four rungs
/// (Initiative → Epic → Story → Sub-task).
pub const MAX_DEPTH: usize = 5;

/// How far a recursive walk will go before it concludes the data is corrupt.
///
/// Well above [`MAX_DEPTH`] on purpose: this is not the product rule, it is the
/// backstop that keeps a cycle already in the table from hanging the server. A
/// walk that reaches this has found something [`check_reparent`] should have made
/// impossible.
const WALK_LIMIT: i64 = 64;

/// A row of a recursive walk.
#[derive(Debug, FromRow)]
struct Walked {
    id: String,
}

/// A card's ancestors, starting with the card itself and ending at the root.
///
/// The card is included deliberately: every caller wants either "is X in here"
/// (where self-parenting is the degenerate case that must be caught) or "how deep
/// is this" (where the card counts as 1).
pub async fn ancestor_ids(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
) -> AppResult<Vec<String>> {
    let rows = sqlx::query_as::<_, Walked>(
        "WITH RECURSIVE chain(id, parent_id, distance) AS ( \
             SELECT id, parent_id, 0 FROM cards WHERE id = ? \
             UNION ALL \
             SELECT c.id, c.parent_id, chain.distance + 1 \
               FROM cards c JOIN chain ON c.id = chain.parent_id \
              WHERE chain.distance < ? \
         ) \
         SELECT id FROM chain ORDER BY distance",
    )
    .bind(card_id)
    .bind(WALK_LIMIT)
    .fetch_all(&mut *tx)
    .await?;

    let ids: Vec<String> = rows.into_iter().map(|row| row.id).collect();

    // Reaching the limit means the walk never found a root — which, since
    // `check_reparent` is the only way to set a parent, means a cycle got in
    // some other way. Refusing to answer is right: any number this returns is
    // meaningless, and silently truncating would let the caller build on it.
    if ids.len() > usize::try_from(WALK_LIMIT).unwrap_or(usize::MAX) {
        return Err(AppError::internal(anyhow::anyhow!(
            "card {card_id} has an ancestor chain longer than {WALK_LIMIT}: the tree contains a cycle"
        )));
    }

    Ok(ids)
}

/// A card's descendants, including the card itself.
pub async fn descendant_ids(
    tx: &mut sqlx::SqliteConnection,
    card_id: &str,
) -> AppResult<Vec<String>> {
    let rows = sqlx::query_as::<_, Walked>(
        "WITH RECURSIVE sub(id, depth) AS ( \
             SELECT id, 1 FROM cards WHERE id = ? \
             UNION ALL \
             SELECT c.id, sub.depth + 1 FROM cards c JOIN sub ON c.parent_id = sub.id \
              WHERE sub.depth < ? \
         ) \
         SELECT id FROM sub ORDER BY depth",
    )
    .bind(card_id)
    .bind(WALK_LIMIT)
    .fetch_all(&mut *tx)
    .await?;

    Ok(rows.into_iter().map(|row| row.id).collect())
}

/// How deep a card sits. A root is 1.
pub async fn depth_of(tx: &mut sqlx::SqliteConnection, card_id: &str) -> AppResult<usize> {
    Ok(ancestor_ids(tx, card_id).await?.len())
}

/// How tall a card's subtree is. A leaf is 1.
pub async fn subtree_height(tx: &mut sqlx::SqliteConnection, card_id: &str) -> AppResult<usize> {
    let height: Option<i64> = sqlx::query_scalar(
        "WITH RECURSIVE sub(id, depth) AS ( \
             SELECT id, 1 FROM cards WHERE id = ? \
             UNION ALL \
             SELECT c.id, sub.depth + 1 FROM cards c JOIN sub ON c.parent_id = sub.id \
              WHERE sub.depth < ? \
         ) \
         SELECT MAX(depth) FROM sub",
    )
    .bind(card_id)
    .bind(WALK_LIMIT)
    .fetch_one(&mut *tx)
    .await?;

    // A card that does not exist has no height; callers only ask about cards
    // they have already loaded, so treat the absurd case as 1 rather than
    // inventing an error nobody can act on.
    Ok(usize::try_from(height.unwrap_or(1)).unwrap_or(1))
}

/// The hierarchy level a card sits on, via its card type.
///
/// The level lives on `card_types`, not on `cards`: a card's rung is a property
/// of what kind of thing it is, which is what makes "Epic" a row rather than a
/// concept in the code.
pub async fn level_of(tx: &mut sqlx::SqliteConnection, card_id: &str) -> AppResult<i64> {
    sqlx::query_scalar(
        "SELECT ct.level FROM cards c JOIN card_types ct ON c.type_id = ct.id WHERE c.id = ?",
    )
    .bind(card_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)
}

/// Whether a reparent is legal, with a message saying why not.
///
/// # The four rules
///
/// 1. **Same project.** A parent in another project would make the tree span
///    projects, and every roll-up, breadcrumb and board scope would have to
///    decide what that means. It means nothing useful.
/// 2. **`parent.level > child.level`** — ADR 0002's *only* structural rule. An
///    Epic may hold a Story; a Story may not hold an Epic; and a Story may not
///    hold another Story, because two cards on the same rung are siblings by
///    definition.
/// 3. **No cycles.** Walking up from the new parent must never reach the card.
/// 4. **[`MAX_DEPTH`].** The new parent's depth plus the card's own subtree
///    height must fit — moving a card *moves everything under it*, so a shallow
///    parent is not enough on its own.
///
/// Every failure is a [`AppError::Conflict`] (409) with a sentence a human can
/// act on, because every one of these is reachable by dragging a card onto
/// another card and the user needs to know which rule they hit.
pub async fn check_reparent(
    tx: &mut sqlx::SqliteConnection,
    card: &Card,
    new_parent: &Card,
) -> AppResult<()> {
    if new_parent.id == card.id {
        return Err(AppError::Conflict(
            "A card cannot be its own parent.".to_owned(),
        ));
    }

    if new_parent.project_id != card.project_id {
        return Err(AppError::Conflict(format!(
            "{} and {} are in different projects. Move the card to the other project first.",
            card.key, new_parent.key
        )));
    }

    // Cycles first: every check after this walks the tree, and a walk through a
    // cycle is exactly what the WALK_LIMIT guard is there to survive.
    let ancestors = ancestor_ids(&mut *tx, &new_parent.id).await?;
    if ancestors.iter().any(|id| id == &card.id) {
        return Err(AppError::Conflict(format!(
            "{} is already somewhere above {} in the hierarchy, so this would make a loop.",
            card.key, new_parent.key
        )));
    }

    let child_level = level_of(&mut *tx, &card.id).await?;
    let parent_level = level_of(&mut *tx, &new_parent.id).await?;

    if parent_level <= child_level {
        return Err(AppError::Conflict(format!(
            "{} sits at hierarchy level {parent_level} and {} at level {child_level}. A parent \
             must be at a higher level than its child.",
            new_parent.key, card.key
        )));
    }

    // The new parent's depth, plus the whole subtree that travels with the card.
    // Checking the card alone would let a five-deep branch be hung off a
    // four-deep parent.
    let parent_depth = ancestors.len();
    let height = subtree_height(&mut *tx, &card.id).await?;

    if parent_depth + height > MAX_DEPTH {
        return Err(AppError::Conflict(format!(
            "That would nest cards {} levels deep; the limit is {MAX_DEPTH}.",
            parent_depth + height
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // The walks and the guards are exercised end-to-end against a real tree in
    // `backend/tests/domain.rs`, which is where the card fixtures live. Testing
    // them here would mean rebuilding the whole project/type/status fixture in
    // this module for no extra coverage.
}
