//! Property and fuzz tests for AQL.
//!
//! Two invariants, hammered:
//!
//! 1. **The parser never panics.** Whatever bytes arrive — unbalanced quotes and
//!    parens, huge nesting, unicode, SQL metacharacters, `'; DROP TABLE`,
//!    unterminated functions — it returns `Ok` or `Err`, never a panic. A
//!    query language is an attacker's front door; a panic there is a denial of
//!    service.
//!
//! 2. **User data never becomes SQL.** The compiled SQL string is *identical*
//!    whatever value the user supplies — only the bind list changes. If a value
//!    ever leaked into the SQL text, two different values would produce two
//!    different SQL strings, and this test would see it. Backed up by an
//!    execution test: an injection payload matches as a literal string and the
//!    `cards` table is still there afterwards, so no second statement ran.
//!
//! Randomness is proptest's own deterministic RNG (no `rand` dependency), plus a
//! fixed adversarial corpus so the nastiest inputs are always exercised.

use atlas::aql::{self, CompileCtx};
use atlas::auth::role::Role;
use atlas::auth::user::{self, NewUser};
use atlas::db::{self, Db};
use atlas::domain::card::{self, NewCard, Placement};
use atlas::domain::config;
use atlas::domain::template::{self, Template};
use atlas::test_support::TempDb;
use chrono::{SubsecRound, Utc};
use proptest::prelude::*;

/// A compile context that sees everything, so the fuzz targets the query, not
/// the accessibility scoping.
fn ctx() -> CompileCtx {
    CompileCtx {
        viewer_id: "u1".to_owned(),
        viewer_is_admin: true,
        current_user_id: "u1".to_owned(),
        now: Utc::now(),
        limit: 50,
        offset: 0,
    }
}

/// Compiles `summary ~ "<payload>"` with the payload safely quoted, returning the
/// SQL and the bind count. Returns `None` if the harness query does not compile,
/// which is fine — the point is the ones that do.
fn compile_payload(payload: &str) -> Option<(String, usize)> {
    let escaped = payload.replace('\\', "\\\\").replace('"', "\\\"");
    let source = format!("summary ~ \"{escaped}\"");
    compile_source(&source)
}

/// Compiles an arbitrary AQL source, returning the SQL and bind count, or `None`
/// if it does not parse/compile.
fn compile_source(source: &str) -> Option<(String, usize)> {
    let query = aql::parse(source).ok()?;
    let compiled = aql::compile::compile(&query, &ctx()).ok()?;
    Some((compiled.sql, compiled.binds.len()))
}

/// The number of `?` placeholders equals the number of binds — the structural
/// half of "every value is a bind and nothing else is".
fn balanced(sql: &str, binds: usize) -> bool {
    sql.matches('?').count() == binds
}

// ---------------------------------------------------------------------------
// The adversarial corpus — always run, deterministically.
// ---------------------------------------------------------------------------

const ADVERSARIAL: &[&str] = &[
    "",
    "   ",
    "'",
    "\"",
    "\"unterminated",
    "'; DROP TABLE cards; --",
    "status = '; DROP TABLE cards; --'",
    "summary ~ \"'; DROP TABLE cards; --\"",
    "status = Done; DELETE FROM users",
    "(((((((((((((((((((((((((((((((",
    ")))))))))))))))))))))))))))))))",
    "status = (((((Done",
    "status IN (",
    "status IN ()",
    "NOT NOT NOT NOT NOT NOT status = Done",
    "a AND AND b",
    "AND OR NOT",
    "status = = = =",
    "currentUser(",
    "startOfWeek(",
    "startOfWeek(-1w",
    "membersOf(((",
    "filter = ",
    "filter = filter = filter",
    "status WAS WAS WAS",
    "ORDER BY BY BY",
    "ORDER BY",
    "\0\0\0\0",
    "status = \u{1F4A9}",
    "日本語 = テスト",
    "summary ~ \"café ☕ 日本語 \\n \\t\"",
    "= = = = =",
    "status\u{202e}= Done",
    "1 = 1",
    "estimate > 99999999999999999999999999",
    "due < startOfWeek(999999999999999999w)",
    "key = ATLAS-1 OR key = ATLAS-2 OR key = ATLAS-3",
];

/// The corpus plus the pathological repeated inputs, which cannot be `const`.
fn adversarial() -> Vec<String> {
    let mut cases: Vec<String> = ADVERSARIAL.iter().map(|s| (*s).to_owned()).collect();
    cases.push("a OR ".repeat(1000));
    cases.push("(".repeat(5000));
    cases.push("status = Done AND ".repeat(500));
    cases.push("NOT ".repeat(1000));
    cases
}

#[test]
fn the_parser_never_panics_on_the_adversarial_corpus() {
    for input in &adversarial() {
        // The assertion is simply that these return rather than panic; a panic
        // here fails the test with the offending input in the backtrace.
        let _ = aql::parse(input);
        if let Ok(query) = aql::parse(input) {
            let _ = aql::compile::compile(&query, &ctx());
            let _ = aql::compile::typecheck(&query, &ctx());
        }
    }
}

#[test]
fn every_compilable_corpus_query_is_balanced_and_single_statement() {
    for input in &adversarial() {
        if let Ok(query) = aql::parse(input)
            && let Ok(compiled) = aql::compile::compile(&query, &ctx())
        {
            assert!(
                balanced(&compiled.sql, compiled.binds.len()),
                "unbalanced for {input:?}: {}",
                compiled.sql
            );
            // Exactly one statement: the compiler never emits a `;`, so a
            // payload can never smuggle a second one in.
            assert_eq!(
                compiled.sql.matches(';').count(),
                0,
                "a semicolon appeared for {input:?}: {}",
                compiled.sql
            );
        }
    }
}

/// The value-invariance property, checked in **every** position a value can ride
/// — not only `summary ~`. A leak in any one path (a field the compiler forgot
/// to bind, a function argument concatenated into a subquery, an `IN` element, a
/// history modifier) would show up as two different SQL strings for two different
/// payloads. Each template holds the SQL structure fixed and varies only the
/// value, so the SQL text must be byte-identical across payloads.
#[test]
fn the_sql_is_value_invariant_in_every_position() {
    // Every value-bearing shape the grammar offers: scalar equality, text match,
    // reference fields, `IN` lists, each set function's arguments, labels,
    // priority ordering, and the full `WAS`/`CHANGED` history family with its
    // FROM/TO/BY/AFTER modifiers. `{V}` is the only thing that changes.
    const TEMPLATES: &[&str] = &[
        r#"summary ~ "{V}""#,
        r#"description ~ "{V}""#,
        r#"text ~ "{V}""#,
        r#"key = "{V}""#,
        r#"key IN ("{V}", "x")"#,
        r#"parent = "{V}""#,
        r#"status = "{V}""#,
        r#"status IN ("{V}", "other")"#,
        r#"status NOT IN ("{V}")"#,
        r#"project = "{V}""#,
        r#"type = "{V}""#,
        r#"resolution = "{V}""#,
        r#"assignee = "{V}""#,
        r#"assignee IN membersOf("{V}")"#,
        r#"key IN linkedCards("{V}")"#,
        r#"key IN linkedCards("A", "{V}")"#,
        r#"labels = "{V}""#,
        r#"labels IN ("{V}", "y")"#,
        r#"priority > "{V}""#,
        r#"status WAS "{V}""#,
        r#"status WAS IN ("{V}", "x")"#,
        r#"status WAS NOT "{V}""#,
        r#"status CHANGED FROM "{V}" TO "y" BY "{V}" AFTER "{V}""#,
        r#"status CHANGED ON "{V}""#,
        r#"status CHANGED DURING ("{V}", "{V}")"#,
        r#"assignee = currentUser() AND key = "{V}""#,
    ];
    // Two payloads that both parse as quoted strings, one benign and one a
    // maximal injection attempt: single quotes, a statement terminator, a
    // comment, LIKE wildcards, and a boolean tautology.
    let payloads = ["AAA", "x'; DROP TABLE cards; -- %_ OR 1=1"];
    for template in TEMPLATES {
        let a = template.replace("{V}", payloads[0]);
        let b = template.replace("{V}", payloads[1]);
        let (Some((sql_a, binds_a)), Some((sql_b, binds_b))) =
            (compile_source(&a), compile_source(&b))
        else {
            panic!("template {template:?} did not compile for both payloads");
        };
        assert_eq!(
            sql_a, sql_b,
            "template {template:?} let the value into the SQL text"
        );
        assert!(balanced(&sql_a, binds_a), "unbalanced: {sql_a}");
        assert!(balanced(&sql_b, binds_b), "unbalanced: {sql_b}");
        assert_eq!(
            sql_a.matches(';').count(),
            0,
            "a semicolon appeared for {template:?}: {sql_a}"
        );
    }
}

/// A predicate tree deeper than the compiler's node-depth bound is refused with
/// an ordinary error, never a stack overflow.
///
/// `AND`/`OR` chains are built *iteratively* by the parser, so they slip past its
/// paren/`NOT` recursion guard (`MAX_DEPTH`) entirely and are the real way a
/// client can hand the compiler an arbitrarily deep tree. The token cap bounds
/// how deep, and the compiler's own [`aql::compile`] depth check refuses whatever
/// gets past it — this pins both ends of that boundary.
#[test]
fn a_pathologically_deep_and_chain_errors_instead_of_overflowing() {
    // Comfortably inside the bound: parses and compiles.
    let shallow = vec!["status = Done"; 400].join(" AND ");
    let query = aql::parse(&shallow).expect("a 400-deep chain parses");
    assert!(
        aql::compile::compile(&query, &ctx()).is_ok(),
        "a 400-deep chain should compile"
    );

    // Past the bound: still parses (it is within the token cap), but the compiler
    // refuses it rather than recursing into a stack overflow.
    let deep = vec!["status = Done"; 600].join(" AND ");
    let query = aql::parse(&deep).expect("a 600-deep chain parses within the token cap");
    assert!(
        aql::compile::compile(&query, &ctx()).is_err(),
        "a 600-deep chain must be refused, not compiled"
    );
    // The typechecker (which `POST /search/validate` runs) must refuse it too, on
    // the same bound, so validation and execution agree.
    assert!(
        aql::compile::typecheck(&query, &ctx()).is_err(),
        "typecheck must refuse the 600-deep chain as well"
    );
}

// ---------------------------------------------------------------------------
// Properties over random input.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Any string at all: parse must return, never panic.
    #[test]
    fn parse_never_panics(input in ".*") {
        let _ = aql::parse(&input);
    }

    /// Random token-salad from the real vocabulary: still no panic, and anything
    /// that compiles is balanced and single-statement.
    #[test]
    fn token_salad_never_panics_and_compiles_safely(
        parts in prop::collection::vec(fragment(), 0..40)
    ) {
        let source = parts.join(" ");
        if let Ok(query) = aql::parse(&source)
            && let Ok(compiled) = aql::compile::compile(&query, &ctx())
        {
            prop_assert!(balanced(&compiled.sql, compiled.binds.len()), "{}", compiled.sql);
            prop_assert_eq!(compiled.sql.matches(';').count(), 0);
        }
    }

    /// The core invariant: the SQL text does not depend on the value. Two
    /// different payloads compile to the *same* SQL — the values differ only in
    /// the bind list. If user data ever reached the SQL string, these would
    /// diverge.
    #[test]
    fn the_sql_is_the_same_whatever_the_value_is(a in ".{0,64}", b in ".{0,64}") {
        if let (Some((sql_a, binds_a)), Some((sql_b, binds_b))) =
            (compile_payload(&a), compile_payload(&b))
        {
            prop_assert_eq!(&sql_a, &sql_b, "the value {:?} vs {:?} changed the SQL", a, b);
            prop_assert!(balanced(&sql_a, binds_a));
            prop_assert!(balanced(&sql_b, binds_b));
        }
    }
}

/// A fragment of AQL, sampled to build deeper random queries than `.*` reaches.
fn fragment() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "status",
        "priority",
        "summary",
        "assignee",
        "resolution",
        "labels",
        "key",
        "created",
        "=",
        "!=",
        ">",
        ">=",
        "<",
        "<=",
        "~",
        "!~",
        "IN",
        "NOT IN",
        "IS",
        "IS NOT",
        "WAS",
        "CHANGED",
        "AND",
        "OR",
        "NOT",
        "EMPTY",
        "NULL",
        "(",
        ")",
        ",",
        "Done",
        "\"In Progress\"",
        "High",
        "currentUser()",
        "startOfWeek(-1w)",
        "now()",
        "ORDER",
        "BY",
        "ASC",
        "DESC",
        "'; DROP TABLE cards; --",
        "filter",
        "membersOf(\"X\")",
    ])
}

// ---------------------------------------------------------------------------
// Execution: an injection payload is data, and no second statement runs.
// ---------------------------------------------------------------------------

async fn seed() -> (Db, TempDb, atlas::auth::User) {
    let temp = TempDb::new();
    let db = Db::connect(&temp.config()).await.expect("open db");
    db::migrate::run(&db).await.expect("migrate");
    let now = Utc::now().trunc_subsecs(6);

    let mut tx = db.begin_write().await.expect("tx");
    let admin = user::insert(
        &mut tx,
        &NewUser {
            username: "admin".to_owned(),
            email: None,
            display_name: "Admin".to_owned(),
            password_hash: "x".to_owned(),
            role: Role::Admin,
            must_change_password: false,
        },
        now,
    )
    .await
    .expect("insert admin");

    let project = template::create_project(
        &mut tx,
        Template::Blank,
        "ATLAS",
        "Atlas",
        None,
        Some(&admin.id),
        now,
    )
    .await
    .expect("project");
    let card_type = config::default_card_type_tx(&mut tx, &project.id)
        .await
        .expect("type lookup")
        .expect("a default type");

    for summary in [
        "an ordinary card",
        "a card containing '; DROP TABLE cards; -- verbatim",
    ] {
        card::create(
            &mut tx,
            &project,
            &NewCard {
                type_id: card_type.id.clone(),
                parent_id: None,
                summary: summary.to_owned(),
                description: None,
                status_id: None,
                priority_id: None,
                assignee_id: None,
                reporter_id: None,
                due_date: None,
                start_date: None,
                estimate: None,
                placement: Placement::Bottom,
            },
            &admin.id,
            now,
        )
        .await
        .expect("card");
    }
    tx.commit().await.expect("commit");

    (db, temp, admin)
}

async fn card_count(db: &Db) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM cards")
        .fetch_one(db.reader())
        .await
        .expect("count")
}

#[tokio::test]
async fn injection_payloads_are_treated_as_data_not_statements() {
    let (db, _temp, admin) = seed().await;
    let before = card_count(&db).await;
    assert_eq!(before, 2);

    // Each payload matched with `~` must return exactly the number of seeded
    // cards whose summary literally contains it — proving it is compared as
    // data, including the SQL-wildcard `%` (LIKE-escaped, so it matches a
    // literal percent, of which there are none).
    let summaries = [
        "an ordinary card",
        "a card containing '; DROP TABLE cards; -- verbatim",
    ];
    let payloads = [
        "'; DROP TABLE cards; --",
        "DROP TABLE cards",
        "ordinary",
        "no-such-text-anywhere",
        "' OR '1'='1",
        "%",
    ];

    for payload in payloads {
        let escaped = payload.replace('\\', "\\\\").replace('"', "\\\"");
        let source = format!("summary ~ \"{escaped}\"");
        let results = aql::search(&db, &admin, Utc::now(), &source, 50, 0)
            .await
            .unwrap_or_else(|e| panic!("payload {payload:?} failed to run: {e:?}"));

        // The ground truth the compiled LIKE must reproduce.
        let literal =
            i64::try_from(summaries.iter().filter(|s| s.contains(payload)).count()).unwrap();
        assert_eq!(
            results.total, literal,
            "payload {payload:?} did not match as a literal"
        );
    }

    // The table is still there, with the same rows: no `DROP`, no `DELETE`, no
    // second statement ever executed.
    assert_eq!(
        card_count(&db).await,
        before,
        "a statement escaped the bind boundary"
    );
    db.close().await;
}
