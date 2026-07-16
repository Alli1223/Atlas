//! Lexicographic card ranking, over `fractional_index` 2.0.2.
//!
//! # Why fractional indexing rather than an integer `position`
//!
//! An integer position column forces every row after the drop point to be
//! renumbered on each reorder: O(n) writes per drag, and a deadlock magnet under
//! concurrency. A fractional index instead computes a key *strictly between* the
//! two neighbours the card was dropped between, so a reorder is **one UPDATE of
//! one row**. See `docs/research/rust-stack.md` §7.
//!
//! # The ordering guarantee
//!
//! [`Rank`] wraps the hex stringification of a `FractionalIndex`. That encoding
//! is fixed-width (two lowercase hex chars per byte) and the hex alphabet
//! `0-9a-f` is ascending in ASCII, so **byte order and string order agree**.
//! Consequently a `TEXT` column sorts correctly under SQLite's default `BINARY`
//! collation with a plain `ORDER BY rank` — no custom collation required.
//!
//! Two things break that guarantee, and both are silent:
//!
//! - declaring the column `COLLATE NOCASE` (or any non-`BINARY` collation);
//! - storing a rank that did not come from this module.
//!
//! Hence [`Rank::parse`] validates on the way in, and `Decode` validates on the
//! way out of the database.
//!
//! # Key growth
//!
//! Keys grow by roughly one byte per insertion into the *same* gap. Thousands of
//! operations at one spot before it matters, but it is unbounded: monitor
//! `MAX(LENGTH(rank))` and rebalance offline if it crosses ~50.

use std::fmt;
use std::str::FromStr;

use fractional_index::FractionalIndex;
use serde::{Deserialize, Serialize};
use sqlx::database::Database;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::{Decode, Encode, Sqlite, Type};

/// Why a rank could not be produced or read.
#[derive(Debug, thiserror::Error)]
pub enum RankError {
    /// The stored text is not a valid fractional index.
    #[error("invalid rank encoding: {0}")]
    Decode(String),

    /// `between` was given bounds that are equal, or in the wrong order.
    ///
    /// In practice this means the client's view is stale: the neighbours it
    /// named have since been reordered. It is a 409, not a 500.
    #[error(
        "cannot generate a rank between {before} and {after}: \
         the neighbours are equal or out of order (refetch and retry)"
    )]
    OutOfOrder {
        /// The lower bound the caller supplied.
        before: String,
        /// The upper bound the caller supplied.
        after: String,
    },
}

/// A card's sort key: an order-preserving, hex-encoded fractional index.
///
/// Ordering is the derived lexicographic ordering of the inner `String`, which
/// is exactly the ordering SQLite applies to the `TEXT` column.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rank(String);

impl Rank {
    /// The rank for the first card in an empty list.
    pub fn first() -> Self {
        Self::from_index(&FractionalIndex::default())
    }

    /// A rank that sorts strictly after `other`.
    pub fn after(other: &Self) -> Self {
        Self::from_index(&FractionalIndex::new_after(&other.to_index()))
    }

    /// A rank that sorts strictly before `other`.
    pub fn before(other: &Self) -> Self {
        Self::from_index(&FractionalIndex::new_before(&other.to_index()))
    }

    /// A rank that sorts strictly between `before` and `after`.
    ///
    /// This is the drag-and-drop primitive. The `Option`s are the list edges:
    ///
    /// | `before` | `after` | meaning                       |
    /// |----------|---------|-------------------------------|
    /// | `None`   | `None`  | the list was empty            |
    /// | `None`   | `Some`  | dropped at the head           |
    /// | `Some`   | `None`  | dropped at the tail           |
    /// | `Some`   | `Some`  | dropped between two cards     |
    ///
    /// # Errors
    ///
    /// [`RankError::OutOfOrder`] when `before >= after`. There is no key
    /// strictly between two equal keys, so this cannot be made infallible: the
    /// honest signal is a 409 telling the client to refetch its neighbours.
    pub fn between(before: Option<&Self>, after: Option<&Self>) -> Result<Self, RankError> {
        let lower = before.map(Self::to_index);
        let upper = after.map(Self::to_index);

        FractionalIndex::new(lower.as_ref(), upper.as_ref())
            .as_ref()
            .map(Self::from_index)
            .ok_or_else(|| RankError::OutOfOrder {
                before: before.map_or_else(|| "<start>".to_owned(), |r| r.0.clone()),
                after: after.map_or_else(|| "<end>".to_owned(), |r| r.0.clone()),
            })
    }

    /// Parses a rank previously produced by this module.
    ///
    /// # Errors
    ///
    /// [`RankError::Decode`] if `text` is not a valid fractional index.
    pub fn parse(text: &str) -> Result<Self, RankError> {
        // Round-trip through the real decoder rather than merely checking the
        // alphabet: a hex string without the terminator byte is well-formed hex
        // and still not a valid index.
        FractionalIndex::from_string(text)
            .map(|index| Self::from_index(&index))
            .map_err(|err| RankError::Decode(err.to_string()))
    }

    /// The rank as it is stored in the database.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_index(index: &FractionalIndex) -> Self {
        Self(index.to_string())
    }

    /// Infallible because `Rank` is only ever constructed from a valid index.
    fn to_index(&self) -> FractionalIndex {
        FractionalIndex::from_string(&self.0).expect("a Rank always holds a valid fractional index")
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Rank {
    type Err = RankError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Rank {
    type Error = RankError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Rank> for String {
    fn from(rank: Rank) -> Self {
        rank.0
    }
}

impl AsRef<str> for Rank {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// --- sqlx integration: store as TEXT, validate on read ----------------------

impl Type<Sqlite> for Rank {
    fn type_info() -> <Sqlite as Database>::TypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &<Sqlite as Database>::TypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Rank {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <String as Encode<'q, Sqlite>>::encode(self.0.clone(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Rank {
    fn decode(value: <Sqlite as Database>::ValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <String as Decode<'r, Sqlite>>::decode(value)?;
        Ok(Self::parse(&text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_sorts_after_and_before_sorts_before() {
        let first = Rank::first();
        let after = Rank::after(&first);
        let before = Rank::before(&first);

        assert!(before < first);
        assert!(first < after);
        assert!(before < after);
    }

    #[test]
    fn between_lands_strictly_inside_the_gap() {
        let a = Rank::first();
        let b = Rank::after(&a);
        let mid = Rank::between(Some(&a), Some(&b)).unwrap();

        assert!(a < mid, "{a} < {mid}");
        assert!(mid < b, "{mid} < {b}");
    }

    #[test]
    fn between_handles_all_four_edge_combinations() {
        // Empty list.
        let only = Rank::between(None, None).unwrap();
        assert_eq!(only, Rank::first());

        // Head of the list.
        let head = Rank::between(None, Some(&only)).unwrap();
        assert!(head < only);

        // Tail of the list.
        let tail = Rank::between(Some(&only), None).unwrap();
        assert!(only < tail);

        // Between two cards.
        let mid = Rank::between(Some(&only), Some(&tail)).unwrap();
        assert!(only < mid && mid < tail);
    }

    #[test]
    fn between_equal_bounds_is_a_stale_client_not_a_panic() {
        let a = Rank::first();
        let err = Rank::between(Some(&a), Some(&a)).unwrap_err();
        assert!(matches!(err, RankError::OutOfOrder { .. }));
    }

    #[test]
    fn between_reversed_bounds_is_rejected() {
        let a = Rank::first();
        let b = Rank::after(&a);
        // Deliberately the wrong way round.
        let err = Rank::between(Some(&b), Some(&a)).unwrap_err();
        assert!(matches!(err, RankError::OutOfOrder { .. }));
    }

    #[test]
    fn ranks_round_trip_through_text() {
        let rank = Rank::after(&Rank::first());
        let text = rank.as_str().to_owned();

        assert_eq!(Rank::parse(&text).unwrap(), rank);
        assert_eq!(text.parse::<Rank>().unwrap(), rank);
        assert_eq!(Rank::try_from(text.clone()).unwrap(), rank);
        assert_eq!(String::from(rank.clone()), text);
        assert_eq!(rank.to_string(), text);
    }

    #[test]
    fn ranks_round_trip_through_json() {
        let rank = Rank::first();
        let json = serde_json::to_string(&rank).unwrap();
        // `serde(transparent)` means a bare string, not `{"0": "..."}`.
        assert_eq!(json, format!("\"{}\"", rank.as_str()));
        assert_eq!(serde_json::from_str::<Rank>(&json).unwrap(), rank);
    }

    #[test]
    fn garbage_is_rejected_rather_than_silently_accepted() {
        // Not hex at all.
        assert!(Rank::parse("zzz").is_err());
        // Empty.
        assert!(Rank::parse("").is_err());
        // Well-formed hex, but no terminator byte: this is the case a naive
        // alphabet check would wave through.
        assert!(Rank::parse("0102").is_err());
    }

    #[test]
    fn the_encoding_is_lowercase_fixed_width_hex() {
        // The ordering guarantee rests on this. If the encoding ever changed to
        // variable width or uppercase, `ORDER BY rank` would quietly go wrong.
        let rank = Rank::first();
        assert!(rank.as_str().len().is_multiple_of(2), "{rank}");
        assert!(
            rank.as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{rank}"
        );
    }

    #[test]
    fn string_ordering_agrees_with_the_underlying_index_ordering() {
        // Build a shuffled-ish set of ranks and check that sorting the strings
        // gives the same answer as sorting the indices.
        let a = Rank::first();
        let b = Rank::after(&a);
        let c = Rank::after(&b);
        let mid = Rank::between(Some(&a), Some(&b)).unwrap();

        let mut ranks = vec![c.clone(), a.clone(), mid.clone(), b.clone()];
        ranks.sort();
        assert_eq!(ranks, vec![a, mid, b, c]);
    }
}
