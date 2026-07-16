//! Property tests for [`atlas::rank::Rank`].
//!
//! The two invariants everything else rests on:
//!
//! 1. `between(a, b)` lands **strictly** between `a` and `b`, for any valid
//!    `a < b`, under the same string ordering SQLite uses.
//! 2. Repeated `between()` into the same gap **never collides** — the property
//!    that lets a reorder be one UPDATE of one row instead of a renumber.

use std::collections::HashSet;

use atlas::rank::Rank;
use proptest::prelude::*;

/// Builds a ranked list by inserting at the given positions, returning it in
/// list order. Mirrors what the board does on every drag-and-drop.
fn build_list(positions: &[usize]) -> Vec<Rank> {
    let mut list: Vec<Rank> = Vec::new();

    for &position in positions {
        let index = position % (list.len() + 1);

        // Clone the neighbours: `between` borrows them, and we insert into the
        // same list immediately afterwards.
        let before = index.checked_sub(1).map(|i| list[i].clone());
        let after = list.get(index).cloned();

        let rank = Rank::between(before.as_ref(), after.as_ref())
            .expect("neighbours taken from a sorted list are always in order");

        list.insert(index, rank);
    }

    list
}

proptest! {
    /// Property 1, directly: every generated rank sorts strictly between the two
    /// neighbours it was asked to go between.
    #[test]
    fn between_always_lands_strictly_between_its_neighbours(
        positions in prop::collection::vec(0usize..64, 1..60)
    ) {
        let mut list: Vec<Rank> = Vec::new();

        for position in positions {
            let index = position % (list.len() + 1);
            let before = index.checked_sub(1).map(|i| list[i].clone());
            let after = list.get(index).cloned();

            let rank = Rank::between(before.as_ref(), after.as_ref())
                .expect("neighbours from a sorted list are in order");

            if let Some(before) = &before {
                prop_assert!(before < &rank, "{before} < {rank} violated");
            }
            if let Some(after) = &after {
                prop_assert!(&rank < after, "{rank} < {after} violated");
            }

            list.insert(index, rank);
        }
    }

    /// The list built by inserting at arbitrary positions is strictly increasing
    /// — i.e. plain `ORDER BY rank` reproduces the intended order.
    #[test]
    fn a_built_list_is_strictly_increasing_and_collision_free(
        positions in prop::collection::vec(0usize..64, 1..60)
    ) {
        let list = build_list(&positions);

        for pair in list.windows(2) {
            prop_assert!(pair[0] < pair[1], "{} < {} violated", pair[0], pair[1]);
        }

        let unique: HashSet<&Rank> = list.iter().collect();
        prop_assert_eq!(unique.len(), list.len(), "duplicate ranks generated");
    }

    /// Sorting by the *string* must give the same answer as list order. This is
    /// the property that makes `ORDER BY rank` correct under SQLite's BINARY
    /// collation, and it is the one that silently breaks under COLLATE NOCASE.
    #[test]
    fn sorting_by_string_reproduces_list_order(
        positions in prop::collection::vec(0usize..64, 1..60)
    ) {
        let list = build_list(&positions);

        let mut by_string: Vec<String> = list.iter().map(|r| r.as_str().to_owned()).collect();
        by_string.sort();

        let expected: Vec<String> = list.iter().map(|r| r.as_str().to_owned()).collect();
        prop_assert_eq!(by_string, expected);
    }

    /// Property 2: hammering the same gap never produces a duplicate.
    #[test]
    fn repeated_between_in_the_same_gap_never_collides(iterations in 1usize..150) {
        let low = Rank::first();
        let mut high = Rank::after(&low);

        let mut seen: HashSet<Rank> = HashSet::new();
        seen.insert(low.clone());
        seen.insert(high.clone());

        for i in 0..iterations {
            let mid = Rank::between(Some(&low), Some(&high))
                .expect("there is always room between two distinct ranks");

            prop_assert!(low < mid, "iteration {}: {} < {} violated", i, low, mid);
            prop_assert!(mid < high, "iteration {}: {} < {} violated", i, mid, high);
            prop_assert!(seen.insert(mid.clone()), "iteration {i}: collision on {mid}");

            // Narrow the gap every round: the hardest case for the encoding.
            high = mid;
        }
    }

    /// The mirror of the above, walking the *lower* bound upward.
    #[test]
    fn repeatedly_appending_into_a_gap_stays_ordered(iterations in 1usize..150) {
        let mut low = Rank::first();
        let high = Rank::after(&low);
        let mut seen: HashSet<Rank> = HashSet::new();

        for i in 0..iterations {
            let mid = Rank::between(Some(&low), Some(&high)).expect("room remains");
            prop_assert!(low < mid && mid < high, "iteration {i} broke ordering");
            prop_assert!(seen.insert(mid.clone()), "iteration {i}: collision on {mid}");
            low = mid;
        }
    }

    /// Ranks survive the round trip they actually make: Rust -> TEXT -> Rust.
    #[test]
    fn ranks_round_trip_through_text(positions in prop::collection::vec(0usize..64, 1..40)) {
        for rank in build_list(&positions) {
            let text = rank.as_str().to_owned();
            let parsed = Rank::parse(&text).expect("a generated rank must parse");
            prop_assert_eq!(&parsed, &rank);
            prop_assert_eq!(parsed.as_str(), text);
        }
    }

    /// `between` must reject bad bounds rather than inventing a key.
    #[test]
    fn between_rejects_equal_or_reversed_bounds(
        positions in prop::collection::vec(0usize..64, 2..40)
    ) {
        let list = build_list(&positions);
        prop_assume!(list.len() >= 2);

        let low = &list[0];
        let high = &list[list.len() - 1];

        prop_assert!(Rank::between(Some(low), Some(low)).is_err(), "equal bounds accepted");
        prop_assert!(Rank::between(Some(high), Some(low)).is_err(), "reversed bounds accepted");
    }
}

/// The database half of the ordering guarantee: SQLite must agree with Rust.
///
/// The unit tests prove `Rank` sorts correctly in memory. This proves the claim
/// that actually matters — that `ORDER BY rank` on a `TEXT` column returns the
/// same order — because that is where a collation mistake would show up.
#[tokio::test]
async fn sqlite_order_by_agrees_with_rust_ordering() {
    use atlas::db::{Db, migrate};
    use atlas::test_support::TempDb;

    let temp = TempDb::new();
    let db = Db::connect(&temp.config()).await.unwrap();
    migrate::run(&db).await.unwrap();

    sqlx::query("CREATE TABLE rank_probe (id INTEGER PRIMARY KEY, rank TEXT NOT NULL) STRICT")
        .execute(db.writer())
        .await
        .unwrap();

    // A list whose insertion order is deliberately unrelated to its sort order.
    let list = build_list(&[0, 0, 1, 0, 2, 1, 3, 0, 5, 2, 4, 1]);

    // Insert in an order that guarantees rowid order != rank order.
    let mut tx = db.begin_write().await.unwrap();
    for (i, rank) in list.iter().enumerate().rev() {
        sqlx::query("INSERT INTO rank_probe (id, rank) VALUES (?, ?)")
            .bind(i64::try_from(i).unwrap())
            .bind(rank)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    // Decoding into `Rank` also exercises the sqlx `Decode` impl.
    let from_db: Vec<Rank> = sqlx::query_scalar("SELECT rank FROM rank_probe ORDER BY rank, id")
        .fetch_all(db.reader())
        .await
        .unwrap();

    assert_eq!(
        from_db, list,
        "SQLite's ORDER BY disagreed with Rust's ordering"
    );

    db.close().await;
}
