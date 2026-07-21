//! Project templates: the seed data that makes one engine three products.
//!
//! # The point of this file
//!
//! Everything below is **data**. There is no `if template == Programming` in the
//! domain, no level name in any query, and no status the code knows by name. A
//! template is a list of rows to insert; the engine reads them back and behaves
//! differently because the rows are different, not because it was told which
//! kind of project it is.
//!
//! That is the claim ADR 0002 makes, and this file is where it gets tested for
//! real. [`Template::JobSearch`] is the proof: nine statuses, three of them
//! terminal, a hierarchy of Company → Application → Task, and priorities called
//! "Dream Job" and "Backup". If the model needed one special case to express
//! that, the model would be wrong — a job-search board would be a fork, not a
//! configuration, and `TODO.md`'s "nothing in the core may assume a software
//! workflow" would already be false.
//!
//! Phase 18 wraps a wizard around this. The data is here now because the domain
//! model needs something to be tested against, and because a template that is
//! discovered late is a template the schema was not designed for.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::domain::project::{self, NewProject, Project};
use crate::domain::{EstimationUnit, StatusCategory, config, tag, workflow};
use crate::error::AppResult;

/// One rung of a template's hierarchy.
type LevelSeed = (i64, &'static str);

/// `(name, level, icon, colour, is_default)`.
type CardTypeSeed = (&'static str, i64, &'static str, &'static str, bool);

/// `(name, category, position)`.
type StatusSeed = (&'static str, StatusCategory, i64);

/// `(name, rank, icon, colour)`. Lower rank = more urgent.
type PrioritySeed = (&'static str, i64, &'static str, &'static str);

/// `(name, position)`. Position 1 is the default a done-transition auto-sets.
type ResolutionSeed = (&'static str, i64);

/// Which set of seed rows a new project starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Template {
    /// Software. `[SW]` — the only template that assumes code exists.
    Programming,
    /// A 3D asset pipeline.
    #[serde(rename = "3d-modeling")]
    ThreeDModeling,
    /// A job hunt. The domain-neutrality proof.
    JobSearch,
    /// Three statuses and two levels. Bring your own workflow.
    #[default]
    Blank,
}

impl Template {
    /// The template's spelling in the database and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Programming => "programming",
            Self::ThreeDModeling => "3d-modeling",
            Self::JobSearch => "job-search",
            Self::Blank => "blank",
        }
    }

    /// Every template, for the wizard's picker.
    pub fn all() -> [Self; 4] {
        [
            Self::Programming,
            Self::ThreeDModeling,
            Self::JobSearch,
            Self::Blank,
        ]
    }

    /// A one-line description for the picker.
    pub fn description(self) -> &'static str {
        match self {
            Self::Programming => "Initiatives, epics, stories and sub-tasks, with a review column.",
            Self::ThreeDModeling => {
                "Collections, assets and steps, through a concept-to-approved pipeline."
            }
            Self::JobSearch => {
                "Companies, applications and tasks, from interested through to an offer."
            }
            Self::Blank => "Two levels and three columns. Configure it yourself.",
        }
    }

    /// Whether a project from this template starts with cycles on.
    ///
    /// Only programming. A job hunt has no sprints, and pretending otherwise is
    /// exactly the assumption docs/adr/0004 exists to refuse.
    pub fn cycles_enabled(self) -> bool {
        matches!(self, Self::Programming)
    }

    /// The estimation unit a project from this template starts with.
    ///
    /// `None` for everything but programming. A number nobody asked for is a
    /// field nobody fills in, and reports degrade to counting cards, which is a
    /// perfectly good answer.
    pub fn estimation_unit(self) -> EstimationUnit {
        match self {
            Self::Programming => EstimationUnit::Points,
            _ => EstimationUnit::None,
        }
    }

    /// The hierarchy this template names.
    ///
    /// Compare the three side by side and the model's whole argument is visible:
    /// the same `parent_id` serves an Epic holding a Story, a Collection holding
    /// an Asset, and a Company holding an Application.
    pub fn levels(self) -> &'static [LevelSeed] {
        match self {
            Self::Programming => &[
                (2, "Initiative"),
                (1, "Epic"),
                (0, "Story"),
                (-1, "Sub-task"),
            ],
            Self::ThreeDModeling => &[(2, "Collection"), (1, "Asset"), (0, "Model"), (-1, "Step")],
            // No level 2: a job hunt has companies and applications, and
            // inventing a rung above "Company" to fill the table would be the
            // template bending to the schema rather than the other way round.
            Self::JobSearch => &[(1, "Company"), (0, "Application"), (-1, "Task")],
            Self::Blank => &[(1, "Group"), (0, "Card"), (-1, "Sub-task")],
        }
    }

    /// The card types this template seeds.
    pub fn card_types(self) -> &'static [CardTypeSeed] {
        match self {
            Self::Programming => &[
                ("Initiative", 2, "target", "#8270DB", false),
                ("Epic", 1, "zap", "#904EE2", false),
                ("Story", 0, "bookmark", "#4BADE8", true),
                ("Bug", 0, "bug", "#E5493A", false),
                ("Task", 0, "check-square", "#4BADE8", false),
                ("Sub-task", -1, "git-branch", "#4BADE8", false),
            ],
            Self::ThreeDModeling => &[
                ("Collection", 2, "folder", "#8270DB", false),
                ("Asset", 1, "box", "#904EE2", false),
                ("Model", 0, "shapes", "#4BADE8", true),
                ("Texture", 0, "image", "#E2B203", false),
                ("Rig", 0, "bone", "#6CC3E0", false),
                ("Step", -1, "list-checks", "#7A869A", false),
            ],
            Self::JobSearch => &[
                ("Company", 1, "building", "#904EE2", false),
                ("Application", 0, "file-text", "#4BADE8", true),
                ("Task", -1, "check-square", "#7A869A", false),
            ],
            Self::Blank => &[
                ("Group", 1, "folder", "#904EE2", false),
                ("Card", 0, "square", "#4BADE8", true),
                ("Sub-task", -1, "git-branch", "#7A869A", false),
            ],
        }
    }

    /// The workflow this template seeds.
    ///
    /// Note what the three have in common: **nothing but the three categories**.
    /// Nine statuses, eight statuses, five statuses; different names, different
    /// counts, different meanings. The board, the reports and the resolution
    /// rules work identically across all of them because they only ever ask
    /// which of the three buckets a status is in.
    pub fn statuses(self) -> &'static [StatusSeed] {
        match self {
            Self::Programming => &[
                ("To Do", StatusCategory::Todo, 1),
                ("In Progress", StatusCategory::InProgress, 2),
                ("In Review", StatusCategory::InProgress, 3),
                ("Blocked", StatusCategory::InProgress, 4),
                ("Done", StatusCategory::Done, 5),
            ],
            Self::ThreeDModeling => &[
                ("Concept", StatusCategory::Todo, 1),
                ("Blockout", StatusCategory::InProgress, 2),
                ("Modeling", StatusCategory::InProgress, 3),
                ("UV / Texture", StatusCategory::InProgress, 4),
                ("Rigging", StatusCategory::InProgress, 5),
                ("Render", StatusCategory::InProgress, 6),
                ("Review", StatusCategory::InProgress, 7),
                ("Approved", StatusCategory::Done, 8),
            ],
            // Three terminal columns, and that is the interesting part. Accepted,
            // Rejected and Ghosted are all `done` — the application is over — but
            // they are emphatically not the same outcome, which is exactly what
            // resolutions are for. A model that only had "Done" could not tell
            // them apart, and a model that made them separate categories would
            // have broken the three-bucket rule every report depends on.
            Self::JobSearch => &[
                ("Interested", StatusCategory::Todo, 1),
                ("Applied", StatusCategory::InProgress, 2),
                ("Phone Screen", StatusCategory::InProgress, 3),
                ("Interview", StatusCategory::InProgress, 4),
                ("Take-home", StatusCategory::InProgress, 5),
                ("Offer", StatusCategory::InProgress, 6),
                ("Accepted", StatusCategory::Done, 7),
                ("Rejected", StatusCategory::Done, 8),
                ("Ghosted", StatusCategory::Done, 9),
            ],
            Self::Blank => &[
                ("To Do", StatusCategory::Todo, 1),
                ("In Progress", StatusCategory::InProgress, 2),
                ("Done", StatusCategory::Done, 3),
            ],
        }
    }

    /// The priorities this template seeds. Rank 1 is the most urgent.
    pub fn priorities(self) -> &'static [PrioritySeed] {
        match self {
            Self::Programming => &[
                ("Highest", 1, "chevrons-up", "#CD1317"),
                ("High", 2, "chevron-up", "#E9494A"),
                ("Medium", 3, "equal", "#E2B203"),
                ("Low", 4, "chevron-down", "#2ABB7F"),
                ("Lowest", 5, "chevrons-down", "#57D9A3"),
            ],
            Self::ThreeDModeling => &[
                ("Critical", 1, "chevrons-up", "#CD1317"),
                ("High", 2, "chevron-up", "#E9494A"),
                ("Normal", 3, "equal", "#E2B203"),
                ("Low", 4, "chevron-down", "#2ABB7F"),
            ],
            Self::JobSearch => &[
                ("Dream Job", 1, "star", "#CD1317"),
                ("Strong Fit", 2, "chevron-up", "#E9494A"),
                ("Maybe", 3, "equal", "#E2B203"),
                ("Backup", 4, "chevron-down", "#2ABB7F"),
            ],
            Self::Blank => &[
                ("High", 1, "chevron-up", "#E9494A"),
                ("Medium", 2, "equal", "#E2B203"),
                ("Low", 3, "chevron-down", "#2ABB7F"),
            ],
        }
    }

    /// The resolutions this template seeds.
    ///
    /// **Position 1 is load-bearing**: it is what a move into a done column
    /// auto-sets when the client did not name one (docs/adr §E). So the first
    /// entry has to be the ending that a card most often reaches by simply
    /// arriving in a terminal column.
    pub fn resolutions(self) -> &'static [ResolutionSeed] {
        match self {
            Self::Programming => &[
                ("Done", 1),
                ("Won't Do", 2),
                ("Duplicate", 3),
                ("Cannot Reproduce", 4),
            ],
            Self::ThreeDModeling => &[("Approved", 1), ("Scrapped", 2), ("Deferred", 3)],
            // "Accepted" first because it is the ending the workflow is aimed
            // at, and because a card dragged to the Accepted column with no
            // resolution named should not come out saying "Rejected".
            Self::JobSearch => &[
                ("Accepted", 1),
                ("Rejected", 2),
                ("Ghosted", 3),
                ("Withdrawn", 4),
            ],
            Self::Blank => &[("Done", 1), ("Won't Do", 2)],
        }
    }
}

impl std::fmt::Display for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Creates a project and every config row its template calls for.
///
/// All of it in the caller's transaction. A project that exists with no statuses
/// is a project no card can be created in, and a half-seeded one is worse than
/// none — so either the whole thing lands or nothing does.
///
/// The insert order is not incidental: `card_types` has a composite foreign key
/// to `hierarchy_levels`, so the levels go in first.
pub async fn create_project(
    tx: &mut sqlx::SqliteConnection,
    template: Template,
    key: &str,
    name: &str,
    description: Option<&str>,
    lead_id: Option<&str>,
    now: DateTime<Utc>,
) -> AppResult<Project> {
    let project = project::insert(
        &mut *tx,
        &NewProject {
            key: key.to_owned(),
            name: name.to_owned(),
            description: description.map(ToOwned::to_owned),
            lead_id: lead_id.map(ToOwned::to_owned),
            template: template.as_str().to_owned(),
            cycles_enabled: template.cycles_enabled(),
            estimation_unit: template.estimation_unit(),
        },
        now,
    )
    .await?;

    apply(&mut *tx, template, &project.id, now).await?;

    Ok(project)
}

/// Inserts a template's config rows into an existing project.
///
/// Split out from [`create_project`] because Phase 18's "copy config from
/// project X" and the import path both want it without the project insert.
///
/// `now` is passed in rather than read here so that every row a project is born
/// with shares one timestamp with the project itself — two calls to `Utc::now()`
/// in one transaction would record a project created microseconds before its own
/// tags, which is not a thing that happened.
pub async fn apply(
    tx: &mut sqlx::SqliteConnection,
    template: Template,
    project_id: &str,
    now: DateTime<Utc>,
) -> AppResult<()> {
    // Levels first: card_types' composite FK points at them.
    for (level, level_name) in template.levels() {
        config::insert_level(&mut *tx, project_id, *level, level_name).await?;
    }

    for (name, level, icon, colour, is_default) in template.card_types() {
        config::insert_card_type(
            &mut *tx,
            project_id,
            name,
            Some(icon),
            Some(colour),
            *level,
            *is_default,
        )
        .await?;
    }

    for (name, category, position) in template.statuses() {
        config::insert_status(&mut *tx, project_id, name, *category, *position).await?;
    }

    for (name, rank, icon, colour) in template.priorities() {
        config::insert_priority(&mut *tx, project_id, name, Some(icon), Some(colour), *rank)
            .await?;
    }

    for (name, position) in template.resolutions() {
        config::insert_resolution(&mut *tx, project_id, name, *position).await?;
    }

    // The permissive default workflow: every status, assigned to every card type,
    // so a card of any type may move between any two of the project's statuses.
    // This is what keeps a template's implied moves legal without seeding an edge
    // per pair — a custom workflow, built later with the transition editor, is
    // what enforces a specific path. Seeded after statuses and card types, which
    // it references. See `domain::workflow::seed_default`.
    workflow::seed_default(&mut *tx, project_id, now).await?;

    // The tag presets. The lists live in `domain::tag` rather than here because
    // they are the only seed data with a rule of their own attached — the
    // no-spaces rule — and a preset that violates it should fail beside the
    // validator that would reject it, not two modules away.
    tag::seed_presets(&mut *tx, template, project_id, now).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn templates_round_trip_through_their_wire_spelling() {
        for template in Template::all() {
            let json = serde_json::to_string(&template).unwrap();
            assert_eq!(json, format!("\"{}\"", template.as_str()));
            assert_eq!(serde_json::from_str::<Template>(&json).unwrap(), template);
        }

        // The one that needs an explicit rename: kebab-case would give
        // "three-d-modeling", which nobody would type.
        assert_eq!(Template::ThreeDModeling.as_str(), "3d-modeling");
        assert_eq!(
            serde_json::to_string(&Template::ThreeDModeling).unwrap(),
            "\"3d-modeling\""
        );
    }

    #[test]
    fn every_template_is_internally_consistent() {
        for template in Template::all() {
            let levels: HashSet<i64> = template.levels().iter().map(|(l, _)| *l).collect();
            assert_eq!(
                levels.len(),
                template.levels().len(),
                "{template}: duplicate level number"
            );

            // The composite foreign key would catch this at insert time, but a
            // seed that cannot be inserted should fail here, not in a migration
            // run on somebody's laptop.
            for (name, level, ..) in template.card_types() {
                assert!(
                    levels.contains(level),
                    "{template}: card type {name:?} is at level {level}, which the template \
                     does not define"
                );
            }

            let default_types = template
                .card_types()
                .iter()
                .filter(|(.., is_default)| *is_default)
                .count();
            assert_eq!(
                default_types, 1,
                "{template}: exactly one card type must be the default"
            );

            // Every project needs somewhere for a card to start and somewhere
            // for it to finish, or `create` and the resolution rules have
            // nothing to work with.
            assert!(
                template
                    .statuses()
                    .iter()
                    .any(|(_, c, _)| *c == StatusCategory::Todo),
                "{template}: no todo status"
            );
            assert!(
                template.statuses().iter().any(|(_, c, _)| c.is_done()),
                "{template}: no done status"
            );

            // Position 1 is what a done-transition auto-sets. A template with no
            // resolutions would make every move into a done column a 409.
            assert!(
                !template.resolutions().is_empty(),
                "{template}: no resolutions, so no card could ever be finished"
            );
            assert!(
                template.resolutions().iter().any(|(_, p)| *p == 1),
                "{template}: no resolution at position 1 to auto-set"
            );

            for (kind, names) in [
                (
                    "status",
                    names(template.statuses().iter().map(|(n, ..)| *n)),
                ),
                (
                    "card type",
                    names(template.card_types().iter().map(|(n, ..)| *n)),
                ),
                (
                    "priority",
                    names(template.priorities().iter().map(|(n, ..)| *n)),
                ),
                (
                    "resolution",
                    names(template.resolutions().iter().map(|(n, _)| *n)),
                ),
            ] {
                assert!(
                    names.is_some(),
                    "{template}: duplicate {kind} name — the UNIQUE index would reject the seed"
                );
            }
        }
    }

    /// `Some` if every name is distinct.
    fn names<'a>(iter: impl Iterator<Item = &'a str>) -> Option<HashSet<&'a str>> {
        let mut seen = HashSet::new();
        for name in iter {
            if !seen.insert(name) {
                return None;
            }
        }
        Some(seen)
    }

    #[test]
    fn the_job_search_template_is_the_domain_neutrality_proof() {
        // If this test needs a special case to pass, the model is wrong: a job
        // hunt would be a fork of Atlas rather than a configuration of it, and
        // TODO.md's "nothing in the core may assume a software workflow" would
        // already be false.
        let template = Template::JobSearch;

        let statuses: Vec<&str> = template.statuses().iter().map(|(n, ..)| *n).collect();
        assert_eq!(
            statuses,
            [
                "Interested",
                "Applied",
                "Phone Screen",
                "Interview",
                "Take-home",
                "Offer",
                "Accepted",
                "Rejected",
                "Ghosted",
            ],
            "the requested job-search workflow, in order"
        );

        // Nine statuses over exactly three categories — the model bends, the
        // three buckets do not.
        let done: Vec<&str> = template
            .statuses()
            .iter()
            .filter(|(_, c, _)| c.is_done())
            .map(|(n, ..)| *n)
            .collect();
        assert_eq!(
            done,
            ["Accepted", "Rejected", "Ghosted"],
            "all three endings are terminal; the resolution says which one happened"
        );

        // Company -> Application -> Task, with no level 2 invented to fill the
        // table.
        let levels: Vec<(i64, &str)> = template.levels().to_vec();
        assert_eq!(levels, [(1, "Company"), (0, "Application"), (-1, "Task")]);

        // And no software assumptions leak in.
        assert!(!template.cycles_enabled(), "a job hunt has no sprints");
        assert_eq!(template.estimation_unit(), EstimationUnit::None);
    }

    #[test]
    fn only_the_programming_template_assumes_software() {
        // The `[SW]` rule from TODO.md, asserted rather than trusted.
        for template in Template::all() {
            let software = template == Template::Programming;
            assert_eq!(
                template.cycles_enabled(),
                software,
                "{template}: cycles are a software habit"
            );
            assert_eq!(
                template.estimation_unit() != EstimationUnit::None,
                software,
                "{template}: story points are a software habit"
            );
        }
    }

    #[test]
    fn no_template_exceeds_the_depth_cap() {
        // A template whose hierarchy is deeper than the cap would seed a project
        // in which the deepest level is unreachable — a card at that level could
        // never be parented all the way up.
        for template in Template::all() {
            assert!(
                template.levels().len() <= crate::domain::hierarchy::MAX_DEPTH,
                "{template}: {} levels exceeds the depth cap of {}",
                template.levels().len(),
                crate::domain::hierarchy::MAX_DEPTH
            );
        }
    }
}
