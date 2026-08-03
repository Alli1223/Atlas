//! Smart commits: turning `ATLAS-42 #done #comment fixed it #time 2h 30m` in a commit
//! message (or a PR title/body) into commands that transition a card, comment on it, and
//! log work.
//!
//! This module holds the **parser** ([`parse`]) and its **application** ([`apply`]) against
//! the workflow engine — comment, worklog, transition. Only the last mile, *receiving* the
//! commits over a webhook, lives elsewhere (the API/webhook layer). The parser proper knows
//! nothing about the database; the applier is best-effort and never fails its caller.
//!
//! # The grammar (`docs/research/github-api.md` §9)
//!
//! - **Keys lead the line.** One or more card keys at the *start* of a line, then the
//!   commands. A key anywhere else does not start a smart commit.
//! - **Commands are `#word [arg]`**, split on `#`; an argument runs up to the next `#`.
//!   `#comment fixed the leak` carries a multi-word argument; `#done` carries none.
//! - **A bare key (no `#command`) only *links*** the commit to the card. Requiring a command
//!   to *transition* is deliberate — a commit that merely names a card must not move it.
//! - **Directives are case-insensitive** (`#DONE` == `#done`); `#done`/`#close`/`#resolve`
//!   are conventionally the same move, resolved when applied.
//! - **Time** is `w`/`d`/`h`/`m` tokens (`2w 3d 4h 30m`), summed to whole minutes on a
//!   working calendar — 1w = 5d, 1d = 8h — with any trailing words kept as the worklog note.
//!
//! # Why the key match is shape-only here
//!
//! The scanner recognises the *shape* `LETTER (LETTER|DIGIT)* '-' DIGIT+` — it deliberately
//! does **not** try to be `[A-Z]{2,}-\d+`, which matches `SHA-256`, `UTF-8`, `RFC-7231`.
//! False positives (`SHA-256 broke the build`) parse to a candidate key that simply fails to
//! resolve to a real card when applied, and are dropped there. Validating against the
//! repo's actual project keys is the applier's job, not the parser's.

use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::domain::card::{self, Card, CardPatch};
use crate::domain::workflow::Outcome;
use crate::domain::{StatusCategory, comment, config, workflow};
use crate::error::{AppError, AppResult};

use super::store::{self, NewWorklog};

/// A single command parsed from a smart-commit directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A `#done` / `#in-progress` / `#code-review` directive — the transition or
    /// target-status name, lower-cased. Resolved against the card's workflow when applied.
    Transition(String),
    /// A `#comment <text>` directive.
    Comment(String),
    /// A `#time <2w 3d 4h 30m> [note]` directive, resolved to whole minutes (> 0).
    Time {
        /// The logged duration in whole minutes.
        minutes: i64,
        /// Any words after the duration, kept as the worklog note.
        note: Option<String>,
    },
}

/// One line of a message that led with card keys: the keys, and the commands to apply to
/// each of them. Keys with no commands only *link* the commit to the cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartCommit {
    /// The card keys the line led with, upper-cased and de-duplicated in order.
    pub keys: Vec<String>,
    /// The commands to apply to each key. Empty means "link only".
    pub commands: Vec<Command>,
}

impl SmartCommit {
    /// Whether this is a bare link (keys, but nothing to do).
    #[must_use]
    pub fn is_link_only(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Scans a whole message for smart-commit lines, in order.
///
/// A message is a commit message, a PR title, or a PR body. Each line that leads with at
/// least one card key yields one [`SmartCommit`]; every other line is ignored.
#[must_use]
pub fn parse(message: &str) -> Vec<SmartCommit> {
    message.lines().filter_map(parse_line).collect()
}

/// Parses one line. `Some` only when the line *leads* with at least one card key.
fn parse_line(line: &str) -> Option<SmartCommit> {
    let mut rest = line.trim_start();
    let mut keys: Vec<String> = Vec::new();

    // Consume the run of leading key tokens. The first token that is not a key (or that
    // starts a command) ends the run.
    while !rest.is_empty() && !rest.starts_with('#') {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (token, tail) = rest.split_at(end);
        match card_key(token) {
            Some(key) => {
                if !keys.contains(&key) {
                    keys.push(key);
                }
                rest = tail.trim_start();
            }
            None => break,
        }
    }

    if keys.is_empty() {
        return None;
    }

    Some(SmartCommit {
        keys,
        commands: parse_commands(rest),
    })
}

/// The normalised card key for a token, if it has the shape `LETTER (LETTER|DIGIT)* '-'
/// DIGIT+`. Trailing punctuation (`ATLAS-42,`) is trimmed first.
fn card_key(token: &str) -> Option<String> {
    let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    let (project, number) = token.rsplit_once('-')?;
    let shaped = !project.is_empty()
        && project.starts_with(|c: char| c.is_ascii_alphabetic())
        && project.chars().all(|c| c.is_ascii_alphanumeric())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit());
    shaped.then(|| token.to_ascii_uppercase())
}

/// The first card key embedded anywhere in free text, by shape — for tying a PR to the card
/// its branch was cut from (`feature/ATLAS-42-add-login` → `ATLAS-42`).
///
/// Unlike [`parse`], the key need not lead the text: this scans for the first maximal
/// `LETTER (LETTER|DIGIT)* '-' DIGIT+` run at a word boundary. Whether it resolves to a real
/// card is, as ever, the caller's to check.
#[must_use]
pub fn key_in_branch(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for start in 0..bytes.len() {
        // A key starts at the beginning or just after a non-alphanumeric byte, with a letter.
        let at_boundary = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if !at_boundary || !bytes[start].is_ascii_alphabetic() {
            continue;
        }
        // The project part: letters and digits.
        let mut dash = start + 1;
        while dash < bytes.len() && bytes[dash].is_ascii_alphanumeric() {
            dash += 1;
        }
        if dash >= bytes.len() || bytes[dash] != b'-' {
            continue;
        }
        // The number part: one or more digits.
        let mut end = dash + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > dash + 1 {
            return Some(text[start..end].to_ascii_uppercase());
        }
    }
    None
}

/// Splits the post-keys remainder into commands on `#`. Text before the first `#` (stray
/// prose between the keys and the first command) is ignored.
fn parse_commands(rest: &str) -> Vec<Command> {
    let mut commands = Vec::new();

    for segment in rest.split('#').skip(1) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let (word, arg) = match segment.split_once(char::is_whitespace) {
            Some((word, arg)) => (word, arg.trim()),
            None => (segment, ""),
        };

        match word.to_ascii_lowercase().as_str() {
            "comment" => {
                if !arg.is_empty() {
                    commands.push(Command::Comment(arg.to_owned()));
                }
            }
            "time" => {
                if let Some((minutes, note)) = parse_time_arg(arg) {
                    commands.push(Command::Time { minutes, note });
                }
            }
            other => commands.push(Command::Transition(other.to_owned())),
        }
    }

    commands
}

/// Parses a `#time` argument: leading `w/d/h/m` duration tokens summed to minutes, then any
/// remaining words as the note. `None` if no positive duration leads the argument.
fn parse_time_arg(arg: &str) -> Option<(i64, Option<String>)> {
    let tokens: Vec<&str> = arg.split_whitespace().collect();

    let mut minutes: i64 = 0;
    let mut consumed = 0;
    for token in &tokens {
        match duration_token_minutes(token) {
            Some(m) => {
                minutes = minutes.checked_add(m)?;
                consumed += 1;
            }
            None => break,
        }
    }

    if minutes <= 0 {
        return None;
    }

    let note = (consumed < tokens.len()).then(|| tokens[consumed..].join(" "));
    Some((minutes, note))
}

/// A single `2w` / `3d` / `4h` / `30m` token → minutes, on a 5-day / 8-hour working
/// calendar. `None` if the token is not a duration.
fn duration_token_minutes(token: &str) -> Option<i64> {
    let unit = token.chars().next_back()?;
    let value: i64 = token[..token.len() - unit.len_utf8()].parse().ok()?;
    if value < 0 {
        return None;
    }
    let per_unit = match unit.to_ascii_lowercase() {
        'w' => 5 * 8 * 60,
        'd' => 8 * 60,
        'h' => 60,
        'm' => 1,
        _ => return None,
    };
    value.checked_mul(per_unit)
}

// ---------------------------------------------------------------------------
// Application against the workflow engine
// ---------------------------------------------------------------------------

/// What applying a smart commit to one card did — for logging and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// A `#comment` landed.
    pub commented: bool,
    /// Minutes logged by `#time` directives.
    pub minutes_logged: i64,
    /// The status name the card was moved to, if a transition fired.
    pub transitioned_to: Option<String>,
}

/// Resolves and applies parsed smart commits against the cards of one project.
///
/// Keys that do not resolve to a real card in `project_id` are dropped — this is where the
/// `SHA-256`-shaped false positives disappear. Link-only lines (a bare key) are skipped:
/// recording the commit as a card link is the webhook layer's job, not the applier's. Every
/// action is attributed to the card's creator, since there is no GitHub→Atlas identity map
/// yet. Returns the number of cards touched.
pub async fn apply(
    db: &Db,
    commits: &[SmartCommit],
    project_id: &str,
    now: DateTime<Utc>,
) -> AppResult<usize> {
    let mut touched = 0;
    for commit in commits {
        if commit.is_link_only() {
            continue;
        }
        for key in &commit.keys {
            let Some(card) = card::find_by_key(db, key).await? else {
                continue;
            };
            // Scope to the repo's project: a commit key must name a card in the project the
            // repo is linked to, not just any card that happens to share the shape.
            if card.project_id != project_id {
                continue;
            }
            let applied = apply_to_card(db, &card, &commit.commands, &card.creator_id, now).await?;
            if applied != Applied::default() {
                touched += 1;
            }
        }
    }
    Ok(touched)
}

/// Applies one card's commands, **each in its own transaction** so a rejected transition
/// cannot roll back an already-landed comment, and a mid-move failure cannot leave the card
/// half-transitioned.
///
/// Best-effort: a directive that cannot be honoured — an unknown transition name, a move the
/// workflow forbids, an over-long comment — is skipped, not propagated. A smart commit must
/// never fail the webhook that delivered it.
pub async fn apply_to_card(
    db: &Db,
    card: &Card,
    commands: &[Command],
    actor: &str,
    now: DateTime<Utc>,
) -> AppResult<Applied> {
    let mut applied = Applied::default();

    for command in commands {
        match command {
            Command::Comment(body) => {
                let Ok(body) = comment::validate_body(body) else {
                    continue;
                };
                let mut tx = db.begin_write().await?;
                comment::insert(&mut tx, &card.id, actor, &body, now).await?;
                tx.commit().await?;
                applied.commented = true;
            }
            Command::Time { minutes, note } => {
                let mut tx = db.begin_write().await?;
                store::insert_worklog(
                    &mut tx,
                    &NewWorklog {
                        card_id: &card.id,
                        author_id: Some(actor),
                        minutes: *minutes,
                        note: note.as_deref(),
                        source: "smart-commit",
                    },
                    now,
                )
                .await?;
                tx.commit().await?;
                applied.minutes_logged += *minutes;
            }
            Command::Transition(directive) => {
                if let Some(name) = apply_transition(db, card, directive, actor, now).await? {
                    applied.transitioned_to = Some(name);
                }
            }
        }
    }

    Ok(applied)
}

/// Applies a smart-commit transition directive by mapping it to a status category.
async fn apply_transition(
    db: &Db,
    card: &Card,
    directive: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> AppResult<Option<String>> {
    match directive_category(directive) {
        Some(category) => move_to_category(db, card, category, actor, now).await,
        None => Ok(None),
    }
}

/// Moves a card into the first status of `category`, if the workflow permits it.
///
/// The shared primitive behind a `#done` smart commit and a PR-driven auto-transition (open →
/// In Progress, merge → Done). Returns the status name moved to, or `None` when there is no
/// status in that category, the card is already there, or the workflow refuses the move.
/// Best-effort: a refused move is a `None`, not an error.
pub async fn move_to_category(
    db: &Db,
    card: &Card,
    category: StatusCategory,
    actor: &str,
    now: DateTime<Utc>,
) -> AppResult<Option<String>> {
    let mut tx = db.begin_write().await?;
    let Some(target) =
        config::first_status_in_category_tx(&mut tx, &card.project_id, category).await?
    else {
        return Ok(None);
    };
    if target.id == card.status_id {
        return Ok(None);
    }

    let moved = match workflow::resolve_transition(&mut tx, card, &target.id, Some(actor)).await {
        Ok(Outcome::Via(transition)) => did_move(
            card::execute_transition(
                &mut tx,
                card,
                &transition,
                CardPatch::default(),
                None,
                Some(actor),
                now,
            )
            .await,
        )?,
        Ok(Outcome::Permissive) => {
            let patch = CardPatch {
                status_id: Some(target.id.clone()),
                ..CardPatch::default()
            };
            did_move(card::update(&mut tx, card, &patch, Some(actor), now).await)?
        }
        Err(err) if is_rejection(&err) => false,
        Err(err) => return Err(err),
    };

    if moved {
        tx.commit().await?;
        Ok(Some(target.name))
    } else {
        // Nothing changed — drop the transaction rather than commit an empty one.
        Ok(None)
    }
}

/// Collapses a transition result to moved / did-not-move, swallowing the workflow's own
/// refusals (a validator said no, a condition hid every edge) but never a real fault.
fn did_move<T>(result: AppResult<T>) -> AppResult<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(err) if is_rejection(&err) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Whether an error is the workflow declining a move (skip it) rather than a fault to raise.
fn is_rejection(err: &AppError) -> bool {
    matches!(
        err,
        AppError::Conflict(_) | AppError::Forbidden | AppError::Validation(_)
    )
}

/// Maps a transition directive to the status category it targets. `#done` and its synonyms
/// close the card; `#in-progress` starts it; `#reopen`/`#todo` send it back. A project's
/// bespoke transition names are not resolved here — not acting is safer than guessing.
fn directive_category(directive: &str) -> Option<StatusCategory> {
    match directive {
        "done" | "close" | "closed" | "closes" | "resolve" | "resolved" | "resolves" | "fix"
        | "fixed" | "fixes" | "complete" | "completed" => Some(StatusCategory::Done),
        "in-progress" | "inprogress" | "progress" | "start" | "started" | "doing" | "wip" => {
            Some(StatusCategory::InProgress)
        }
        "todo" | "to-do" | "reopen" | "reopened" | "reopens" | "backlog" => {
            Some(StatusCategory::Todo)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::auth::{Role, now, user};
    use crate::db::migrate;
    use crate::domain::card::{NewCard, Placement};
    use crate::domain::template::{self, Template};
    use crate::test_support::TempDb;

    fn one(message: &str) -> SmartCommit {
        let mut parsed = parse(message);
        assert_eq!(
            parsed.len(),
            1,
            "expected exactly one smart commit in {message:?}"
        );
        parsed.remove(0)
    }

    #[test]
    fn the_documented_examples_parse_as_specified() {
        assert_eq!(
            one("ATLAS-123 #done"),
            SmartCommit {
                keys: vec!["ATLAS-123".to_owned()],
                commands: vec![Command::Transition("done".to_owned())],
            }
        );

        assert_eq!(
            one("ATLAS-123 #comment fixed the leak").commands,
            vec![Command::Comment("fixed the leak".to_owned())]
        );

        assert_eq!(
            one("ATLAS-123 #time 2h 30m").commands,
            vec![Command::Time {
                minutes: 150,
                note: None
            }]
        );

        // Multiple keys, one command applied to all.
        assert_eq!(
            one("ATLAS-123 ATLAS-124 #done").keys,
            vec!["ATLAS-123".to_owned(), "ATLAS-124".to_owned()]
        );

        // Multiple commands, one card.
        assert_eq!(
            one("ATLAS-123 #time 1h #comment wip #done").commands,
            vec![
                Command::Time {
                    minutes: 60,
                    note: None
                },
                Command::Comment("wip".to_owned()),
                Command::Transition("done".to_owned()),
            ]
        );
    }

    #[test]
    fn a_bare_key_links_but_carries_no_command() {
        let commit = one("ATLAS-42 look at this later");
        assert_eq!(commit.keys, vec!["ATLAS-42".to_owned()]);
        assert!(commit.is_link_only());
    }

    #[test]
    fn keys_must_lead_the_line() {
        // A key in the middle of prose does not start a smart commit.
        assert!(parse("Fix the thing for ATLAS-42 #done").is_empty());
    }

    #[test]
    fn directives_are_case_insensitive_and_keys_are_upper_cased() {
        assert_eq!(
            one("atlas-1 #DONE"),
            SmartCommit {
                keys: vec!["ATLAS-1".to_owned()],
                commands: vec![Command::Transition("done".to_owned())],
            }
        );
    }

    #[test]
    fn a_trailing_comma_on_a_key_is_tolerated_and_duplicates_collapse() {
        let commit = one("ATLAS-7, ATLAS-7 #done");
        assert_eq!(commit.keys, vec!["ATLAS-7".to_owned()]);
    }

    #[test]
    fn a_key_shaped_false_positive_parses_but_is_link_only() {
        // `SHA-256` has the shape of a key. It parses to a candidate that carries no
        // command; whether it resolves to a real card is the applier's problem.
        let commit = one("SHA-256 checksum mismatch");
        assert_eq!(commit.keys, vec!["SHA-256".to_owned()]);
        assert!(commit.is_link_only());
        // A word starting with a digit is not a project key, so this is not a smart commit.
        assert!(parse("8-bit rendering fixed").is_empty());
    }

    #[test]
    fn time_units_sum_on_a_working_calendar_and_keep_a_note() {
        assert_eq!(duration("30m"), 30);
        assert_eq!(duration("2h"), 120);
        assert_eq!(duration("3d"), 3 * 8 * 60);
        assert_eq!(duration("1w"), 5 * 8 * 60);
        assert_eq!(duration("1w 1d 1h 1m"), 5 * 8 * 60 + 8 * 60 + 60 + 1);

        // Words after the duration become the note.
        assert_eq!(
            one("ATLAS-1 #time 1h reviewed the PR").commands,
            vec![Command::Time {
                minutes: 60,
                note: Some("reviewed the PR".to_owned())
            }]
        );
    }

    #[test]
    fn an_unparseable_or_zero_time_is_dropped_rather_than_logged() {
        // `card_worklogs.minutes` is CHECK(> 0), so a zero or junk duration must not become
        // a command at all.
        assert!(one("ATLAS-1 #time 0m").commands.is_empty());
        assert!(one("ATLAS-1 #time soon").commands.is_empty());
        assert!(one("ATLAS-1 #time").commands.is_empty());
    }

    #[test]
    fn every_line_is_scanned_independently() {
        let message = "ATLAS-1 #done\nunrelated line\nATLAS-2 #comment on the second card";
        let parsed = parse(message);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].keys, vec!["ATLAS-1".to_owned()]);
        assert_eq!(
            parsed[1].commands,
            vec![Command::Comment("on the second card".to_owned())]
        );
    }

    /// The minutes a lone duration argument resolves to.
    fn duration(arg: &str) -> i64 {
        parse_time_arg(arg).expect("a valid duration").0
    }

    // --- application ---

    /// A migrated database with a `Programming` project (`ATLAS`) and one card in it, plus
    /// the id of the user everything is attributed to.
    async fn fixture() -> (Db, TempDb, Card) {
        let temp = TempDb::new();
        let db = Db::connect(&temp.config()).await.unwrap();
        migrate::run(&db).await.unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let creator = user::insert(
            &mut tx,
            &user::NewUser {
                username: "committer".to_owned(),
                email: None,
                display_name: "Committer".to_owned(),
                password_hash: "x".to_owned(),
                role: Role::Member,
                must_change_password: false,
            },
            now(),
        )
        .await
        .unwrap();

        let project = template::create_project(
            &mut tx,
            Template::Programming,
            "ATLAS",
            "Atlas",
            None,
            None,
            now(),
        )
        .await
        .unwrap();

        // A parentless card skips level validation, so any of the project's types works.
        let type_id: String = sqlx::query_scalar(
            "SELECT id FROM card_types WHERE project_id = ? ORDER BY level DESC, name LIMIT 1",
        )
        .bind(&project.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();

        let card = card::create(
            &mut tx,
            &project,
            &NewCard {
                type_id,
                parent_id: None,
                summary: "Add login".to_owned(),
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
            &creator.id,
            now(),
        )
        .await
        .unwrap();

        tx.commit().await.unwrap();
        (db, temp, card)
    }

    #[test]
    fn directives_map_to_the_right_status_category() {
        assert_eq!(directive_category("done"), Some(StatusCategory::Done));
        assert_eq!(directive_category("resolve"), Some(StatusCategory::Done));
        assert_eq!(
            directive_category("in-progress"),
            Some(StatusCategory::InProgress)
        );
        assert_eq!(directive_category("reopen"), Some(StatusCategory::Todo));
        // A project's bespoke transition name is not guessed at.
        assert_eq!(directive_category("frobnicate"), None);
    }

    #[tokio::test]
    async fn a_comment_directive_lands_a_comment() {
        let (db, _temp, card) = fixture().await;

        let applied = apply_to_card(
            &db,
            &card,
            &[Command::Comment("fixed the leak".to_owned())],
            &card.creator_id,
            now(),
        )
        .await
        .unwrap();

        assert!(applied.commented);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE card_id = ?")
            .bind(&card.id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn a_time_directive_logs_a_worklog() {
        let (db, _temp, card) = fixture().await;

        let applied = apply_to_card(
            &db,
            &card,
            &[Command::Time {
                minutes: 150,
                note: Some("pairing".to_owned()),
            }],
            &card.creator_id,
            now(),
        )
        .await
        .unwrap();

        assert_eq!(applied.minutes_logged, 150);
        let (minutes, source, note): (i64, String, Option<String>) =
            sqlx::query_as("SELECT minutes, source, note FROM card_worklogs WHERE card_id = ?")
                .bind(&card.id)
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(minutes, 150);
        assert_eq!(source, "smart-commit");
        assert_eq!(note.as_deref(), Some("pairing"));
    }

    #[tokio::test]
    async fn a_done_directive_moves_the_card_to_the_done_status() {
        let (db, _temp, card) = fixture().await;

        let applied = apply_to_card(
            &db,
            &card,
            &[Command::Transition("done".to_owned())],
            &card.creator_id,
            now(),
        )
        .await
        .unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let done =
            config::first_status_in_category_tx(&mut tx, &card.project_id, StatusCategory::Done)
                .await
                .unwrap()
                .expect("the Programming template has a Done status");
        let status_id: String = sqlx::query_scalar("SELECT status_id FROM cards WHERE id = ?")
            .bind(&card.id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();

        assert_eq!(status_id, done.id, "the card should have moved to Done");
        assert_eq!(applied.transitioned_to, Some(done.name));
    }

    #[tokio::test]
    async fn apply_touches_only_resolvable_keys_in_the_repo_project() {
        let (db, _temp, card) = fixture().await;

        // A key-shaped false positive that resolves to nothing, and the real card.
        let message = format!(
            "SHA-256 broke the build\n{} #comment on the real card",
            card.key
        );
        let touched = apply(&db, &parse(&message), &card.project_id, now())
            .await
            .unwrap();

        assert_eq!(touched, 1, "only the real card in this project is touched");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE card_id = ?")
            .bind(&card.id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}
