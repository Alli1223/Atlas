//! The per-project access gate: a layer over the whole `/api/v1` tree.
//!
//! # Why this is a layer, and not a check in each handler
//!
//! Because a per-handler check is one forgotten route away from a hole, and the
//! failure mode of forgetting is *silent*: the route simply works for everybody,
//! and nothing anywhere says so. That is precisely the bug this phase exists to
//! close — until it landed, every handler consulted the instance role and
//! nothing else, so any Member could edit any project.
//!
//! [`crate::auth::middleware::authenticate`] made the same argument for the
//! forced-reset gate and reached the same shape. This follows that precedent,
//! and pushes it one step further.
//!
//! # The design constraint, and how it is met
//!
//! A pure path layer cannot do this job on its own. Project routes are keyed on
//! `{key}` of a *project*, but card routes are keyed on `{key}` of a *card*,
//! comment routes on a comment id, config routes on a status/priority/… id —
//! and each of those needs a database lookup before the word "project" even
//! means anything. So the layer does the lookup: [`Scope`] says *how* to get
//! from this route's path parameter to a project, and [`resolve_target`] does it.
//!
//! The property that makes this worth the trouble is **deny by default**:
//!
//! > Every route under `/api/v1` must appear in [`SCOPES`]. A route that does not
//! > appear is refused with a 500 and a loud error log — for everyone, including
//! > instance admins.
//!
//! ## So what happens when someone adds a route in Phase 8 and forgets?
//!
//! **It returns 500 on the first request anybody makes to it**, with an error
//! log naming the route and telling them to classify it. Not "it is wide open" —
//! it does not work at all. The route is dead until it is classified, which is
//! the loudest failure available and the one that cannot be missed: the author
//! hits it the first time they curl their own endpoint.
//!
//! A 500 rather than a 403 deliberately. An unclassified route is a defect in
//! Atlas, not a statement about the caller — and a 403 would blend into ordinary
//! authorisation traffic and could sit unnoticed for months, whereas a 500 is
//! the one status everybody has an alarm on. It fails closed either way; this
//! way it also fails *audibly*.
//!
//! `tests/project_access.rs` closes the loop by enumerating the live OpenAPI
//! document and asserting every route in it is classified, so the 500 is caught
//! in CI rather than in production. That is the same trick
//! `tests/auth_gate_adversarial.rs` plays on the forced-reset gate: derive the
//! route list from the router, never from a hand-maintained constant, because a
//! hand-maintained list of routes to check *is* the per-handler check the layer
//! exists to replace.
//!
//! # Why `MatchedPath` and not `OriginalUri`
//!
//! The forced-reset gate matches [`axum::extract::OriginalUri`] against literal
//! strings, and pays for it: `tests/auth_gate_adversarial.rs` has a whole test
//! devoted to `/api/v1/%75sers`, `/api/v1//users`, `/api/v1/./users` and friends,
//! because a raw path is attacker-shaped text.
//!
//! [`MatchedPath`] is not text, it is the router's own answer to "which route is
//! this" — `/api/v1/projects/{key}/cards`, already resolved, already normalised,
//! percent-decoding and dot-segments already dealt with by the thing whose job
//! that is. There is no string to dress up: if the router did not match the
//! route, the handler does not run either.
//!
//! Under [`axum::Router::nest`] the matched path is the **full** path including
//! the mount prefix (axum accumulates it across nests), which is why [`SCOPES`]
//! is written with `/api/v1` on the front. This is the opposite of
//! `request.uri()`, which the nest strips — the trap
//! [`crate::auth::middleware::is_allowlisted`] documents.
//!
//! # The window this does not close
//!
//! The layer reads through [`Db::reader`] and the handler then writes inside its
//! own `BEGIN IMMEDIATE` transaction, so a grant revoked in between could let one
//! already-authorised request through. The window is sub-millisecond and the
//! attacker must have had the access a moment earlier, so it buys them nothing
//! they did not already have. It is also exactly the property the instance-role
//! guards already have — [`CurrentUser`] is loaded once by `authenticate` and
//! used by the handler afterwards. Closing it would mean re-checking inside every
//! write transaction, which is the "scope every query in the domain layer" design
//! rejected above.

use axum::RequestExt;
use axum::body::Body;
use axum::extract::{MatchedPath, RawPathParams, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::auth::extract::CurrentUser;
use crate::auth::role::Role;
use crate::db::Db;
use crate::domain::member::{self, ProjectRole};
use crate::domain::project::{self, Project};
use crate::error::{AppError, AppResult};

/// Which of the five per-project config tables an `{id}` belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigTable {
    /// `hierarchy_levels`.
    HierarchyLevels,
    /// `card_types`.
    CardTypes,
    /// `statuses`.
    Statuses,
    /// `priorities`.
    Priorities,
    /// `resolutions`.
    Resolutions,
}

impl ConfigTable {
    /// The query taking one of this table's ids to its project's id.
    ///
    /// One `&'static str` per table rather than one query with the table name
    /// spliced in: every SQL string in Atlas is a `&'static str`, which satisfies
    /// sqlx 0.9's `SqlSafeStr` bound with no `AssertSqlSafe` — and the absence of
    /// `AssertSqlSafe` across the codebase is a real signal that no SQL is
    /// assembled at runtime. Five literals are a cheap price for keeping it.
    fn project_query(self) -> &'static str {
        match self {
            Self::HierarchyLevels => "SELECT project_id FROM hierarchy_levels WHERE id = ?",
            Self::CardTypes => "SELECT project_id FROM card_types WHERE id = ?",
            Self::Statuses => "SELECT project_id FROM statuses WHERE id = ?",
            Self::Priorities => "SELECT project_id FROM priorities WHERE id = ?",
            Self::Resolutions => "SELECT project_id FROM resolutions WHERE id = ?",
        }
    }
}

/// What a route is scoped to, and the least project role that may call it.
///
/// Every route under `/api/v1` has one of these, stated in [`SCOPES`]. There is
/// no default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Not project-scoped at all: the instance role is the whole answer.
    ///
    /// Sign-in, user administration, the template list. The handler's own
    /// [`crate::auth::extract::RequireAdmin`] / `RequireMember` does the work;
    /// this layer stands aside.
    Unscoped,

    /// A collection the **handler** filters down to what the caller may see.
    ///
    /// `GET /projects` is the only one today. A list must never 403: an
    /// inaccessible project is simply not in it. A 403 on a list is a bug —
    /// it turns "here is your work" into "you are not allowed to have work".
    ///
    /// The layer still demands a session; the filtering is
    /// [`project::list_for`], which takes the viewer and has no unscoped
    /// sibling to call by mistake.
    SelfFiltered,

    /// `{key}` is a project key.
    Project(ProjectRole),

    /// `{key}` is a **card** key; the project is the one the card lives in.
    Card(ProjectRole),

    /// `{id}` is a comment id; the project is its card's.
    Comment(ProjectRole),

    /// `{id}` is a tag id. A tag with no project is global — see
    /// [`resolve_target`].
    Tag(ProjectRole),

    /// `{id}` is a row in one of the per-project config tables.
    Config(ConfigTable, ProjectRole),

    /// `{id}` is a workflow id; the project is the one that owns it.
    Workflow(ProjectRole),

    /// `{id}` is a transition id; the project is its workflow's.
    Transition(ProjectRole),

    /// `{id}` is a saved board's id; the project is the one that owns it.
    Board(ProjectRole),
}

impl Scope {
    /// The path parameter this scope resolves its project from.
    ///
    /// `None` for the two scopes that resolve nothing.
    ///
    /// The names are the router's, so a route that spells its parameter
    /// differently resolves nothing and is refused — fail-closed, and loudly,
    /// rather than silently unguarded.
    fn param(self) -> Option<&'static str> {
        match self {
            Self::Unscoped | Self::SelfFiltered => None,
            // `/projects/{key}/members/{userId}` and `/cards/{key}/tags/{tagId}`
            // both carry a second parameter; the first is the one that names the
            // thing access is decided on.
            Self::Project(_) | Self::Card(_) => Some("key"),
            Self::Comment(_)
            | Self::Tag(_)
            | Self::Config(..)
            | Self::Workflow(_)
            | Self::Transition(_)
            | Self::Board(_) => Some("id"),
        }
    }

    /// The least project role that may call the route.
    fn min_role(self) -> Option<ProjectRole> {
        match self {
            Self::Unscoped | Self::SelfFiltered => None,
            Self::Project(role)
            | Self::Card(role)
            | Self::Comment(role)
            | Self::Tag(role)
            | Self::Config(_, role)
            | Self::Workflow(role)
            | Self::Transition(role)
            | Self::Board(role) => Some(role),
        }
    }
}

/// **Every route under `/api/v1`, and what it is scoped to.**
///
/// Keyed on `(method, matched path)` — the router's own route template, not the
/// request's text. A route missing from this table is refused with a 500; see
/// the module docs.
///
/// The capability matrix this encodes:
///
/// | role | may |
/// |---|---|
/// | viewer | read cards, comments, tags, config |
/// | member | viewer + create/edit/move/delete cards, comment, tag |
/// | owner | member + project settings, member management, archive, config |
pub(crate) const SCOPES: &[(Method, &str, Scope)] = &[
    // --- auth: instance-level, and login must work with no session at all ---
    (Method::POST, "/api/v1/auth/login", Scope::Unscoped),
    (Method::POST, "/api/v1/auth/logout", Scope::Unscoped),
    (Method::GET, "/api/v1/auth/me", Scope::Unscoped),
    (
        Method::POST,
        "/api/v1/auth/change-password",
        Scope::Unscoped,
    ),
    (Method::GET, "/api/v1/auth/sessions", Scope::Unscoped),
    (
        Method::DELETE,
        "/api/v1/auth/sessions/{id}",
        Scope::Unscoped,
    ),
    // --- users: instance administration, guarded by RequireAdmin ---
    (Method::GET, "/api/v1/users", Scope::Unscoped),
    (Method::POST, "/api/v1/users", Scope::Unscoped),
    (Method::GET, "/api/v1/users/{id}", Scope::Unscoped),
    (Method::PATCH, "/api/v1/users/{id}", Scope::Unscoped),
    (
        Method::POST,
        "/api/v1/users/{id}/deactivate",
        Scope::Unscoped,
    ),
    // --- projects ---
    // The list filters rather than refusing. See `Scope::SelfFiltered`.
    (Method::GET, "/api/v1/projects", Scope::SelfFiltered),
    // Creating a project is an instance capability — there is no project to be a
    // member of yet. RequireMember guards it; the creator becomes its owner.
    (Method::POST, "/api/v1/projects", Scope::Unscoped),
    // Static seed data. Reveals nothing about any project.
    (Method::GET, "/api/v1/project-templates", Scope::Unscoped),
    (
        Method::GET,
        "/api/v1/projects/{key}",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::PATCH,
        "/api/v1/projects/{key}",
        Scope::Project(ProjectRole::Owner),
    ),
    // Also RequireAdmin in the handler: the only hard delete in Atlas.
    (
        Method::DELETE,
        "/api/v1/projects/{key}",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/archive",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/restore",
        Scope::Project(ProjectRole::Owner),
    ),
    // --- project members ---
    // Readable by any member: "who else is on this?" is not privileged, and a
    // viewer who cannot see the member list cannot work out who to ask for more.
    (
        Method::GET,
        "/api/v1/projects/{key}/members",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/members",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/projects/{key}/members/{userId}",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::DELETE,
        "/api/v1/projects/{key}/members/{userId}",
        Scope::Project(ProjectRole::Owner),
    ),
    // --- cards ---
    (
        Method::GET,
        "/api/v1/projects/{key}/cards",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/cards",
        Scope::Project(ProjectRole::Member),
    ),
    (
        Method::GET,
        "/api/v1/cards/{key}",
        Scope::Card(ProjectRole::Viewer),
    ),
    // The body may also carry `projectKey`, moving the card to another project.
    // The layer cannot see a body, so `api::cards::update_card` checks the
    // destination itself — the one handler-side access check in Atlas, and the
    // reason it is called out in that handler's own comment.
    (
        Method::PATCH,
        "/api/v1/cards/{key}",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::DELETE,
        "/api/v1/cards/{key}",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/restore",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/move",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/reparent",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::GET,
        "/api/v1/cards/{key}/children",
        Scope::Card(ProjectRole::Viewer),
    ),
    // --- boards ---
    // The board *data*: reading a project's cards grouped into columns. A view,
    // so Viewer. The nested-board and quick-filter parameters ride the query
    // string, not the path, so the scope is the project either way.
    (
        Method::GET,
        "/api/v1/projects/{key}/board",
        Scope::Project(ProjectRole::Viewer),
    ),
    // Saved board config: reading is Viewer, curating is Member (a board is a
    // saved view, like a tag or a filter — not structural project configuration).
    (
        Method::GET,
        "/api/v1/projects/{key}/boards",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/boards",
        Scope::Project(ProjectRole::Member),
    ),
    (
        Method::GET,
        "/api/v1/boards/{id}",
        Scope::Board(ProjectRole::Viewer),
    ),
    (
        Method::PATCH,
        "/api/v1/boards/{id}",
        Scope::Board(ProjectRole::Member),
    ),
    (
        Method::DELETE,
        "/api/v1/boards/{id}",
        Scope::Board(ProjectRole::Member),
    ),
    (
        Method::GET,
        "/api/v1/cards/{key}/history",
        Scope::Card(ProjectRole::Viewer),
    ),
    // --- comments ---
    (
        Method::GET,
        "/api/v1/cards/{key}/comments",
        Scope::Card(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/comments",
        Scope::Card(ProjectRole::Member),
    ),
    // Project Member gets you as far as the route; the handler then requires you
    // to be the author (edit) or the author-or-instance-admin (delete).
    (
        Method::PATCH,
        "/api/v1/comments/{id}",
        Scope::Comment(ProjectRole::Member),
    ),
    (
        Method::DELETE,
        "/api/v1/comments/{id}",
        Scope::Comment(ProjectRole::Member),
    ),
    // --- project config: reading is Viewer, changing it is Owner ---
    (
        Method::GET,
        "/api/v1/projects/{key}/hierarchy-levels",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/hierarchy-levels",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/hierarchy-levels/{id}",
        Scope::Config(ConfigTable::HierarchyLevels, ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/projects/{key}/card-types",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/card-types",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/card-types/{id}",
        Scope::Config(ConfigTable::CardTypes, ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/projects/{key}/statuses",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/statuses",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/statuses/{id}",
        Scope::Config(ConfigTable::Statuses, ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/projects/{key}/priorities",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/priorities",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/priorities/{id}",
        Scope::Config(ConfigTable::Priorities, ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/projects/{key}/resolutions",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/resolutions",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/resolutions/{id}",
        Scope::Config(ConfigTable::Resolutions, ProjectRole::Owner),
    ),
    // --- tags: a tag is not administration, so Member, like a comment ---
    (
        Method::GET,
        "/api/v1/projects/{key}/tags",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/tags",
        Scope::Project(ProjectRole::Member),
    ),
    (
        Method::PATCH,
        "/api/v1/tags/{id}",
        Scope::Tag(ProjectRole::Member),
    ),
    (
        Method::DELETE,
        "/api/v1/tags/{id}",
        Scope::Tag(ProjectRole::Member),
    ),
    // `domain::tag::merge` refuses to merge across scopes, so authorising the
    // source tag's project authorises the target's too.
    (
        Method::POST,
        "/api/v1/tags/{id}/merge",
        Scope::Tag(ProjectRole::Member),
    ),
    (
        Method::GET,
        "/api/v1/cards/{key}/tags",
        Scope::Card(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/tags",
        Scope::Card(ProjectRole::Member),
    ),
    (
        Method::DELETE,
        "/api/v1/cards/{key}/tags/{tagId}",
        Scope::Card(ProjectRole::Member),
    ),
    // --- workflows: reading is Viewer, editing is Owner (it is configuration) ---
    (
        Method::GET,
        "/api/v1/projects/{key}/workflows",
        Scope::Project(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/projects/{key}/workflows",
        Scope::Project(ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/workflows/{id}",
        Scope::Workflow(ProjectRole::Viewer),
    ),
    (
        Method::PATCH,
        "/api/v1/workflows/{id}",
        Scope::Workflow(ProjectRole::Owner),
    ),
    (
        Method::DELETE,
        "/api/v1/workflows/{id}",
        Scope::Workflow(ProjectRole::Owner),
    ),
    (
        Method::GET,
        "/api/v1/workflows/{id}/transitions",
        Scope::Workflow(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/workflows/{id}/transitions",
        Scope::Workflow(ProjectRole::Owner),
    ),
    (
        Method::PATCH,
        "/api/v1/transitions/{id}",
        Scope::Transition(ProjectRole::Owner),
    ),
    (
        Method::DELETE,
        "/api/v1/transitions/{id}",
        Scope::Transition(ProjectRole::Owner),
    ),
    // --- a card's transitions: reading is Viewer, taking one is Member (a move) ---
    (
        Method::GET,
        "/api/v1/cards/{key}/transitions",
        Scope::Card(ProjectRole::Viewer),
    ),
    (
        Method::POST,
        "/api/v1/cards/{key}/transitions/{id}",
        Scope::Card(ProjectRole::Member),
    ),
    // --- AQL search and saved filters ---
    //
    // None of these are project-scoped at the route level, and that is on
    // purpose: a search spans every project the caller can see, so there is no
    // single `{key}` to decide on. The scoping is instead compiled *into* the
    // query — `crate::aql` ANDs an accessible-projects predicate onto every
    // statement — so a search can never read cards in a project the caller
    // cannot access. The handler's `CurrentUser`/`RequireMember` extractor
    // supplies the identity that predicate is built from.
    //
    // Filters are personal: each `/filters/{id}` handler loads the row and
    // checks `owner_id` against the caller, answering 404 for someone else's. So
    // `{id}` here is not a project key and must not be resolved as one.
    (Method::POST, "/api/v1/search", Scope::Unscoped),
    (Method::POST, "/api/v1/search/validate", Scope::Unscoped),
    (Method::GET, "/api/v1/filters", Scope::Unscoped),
    (Method::POST, "/api/v1/filters", Scope::Unscoped),
    (Method::GET, "/api/v1/filters/{id}", Scope::Unscoped),
    (Method::PATCH, "/api/v1/filters/{id}", Scope::Unscoped),
    (Method::DELETE, "/api/v1/filters/{id}", Scope::Unscoped),
    (Method::GET, "/api/v1/filters/{id}/results", Scope::Unscoped),
    // --- the secrets vault: instance settings, not project config ---
    //
    // `Unscoped`, on purpose. A GitHub PAT or a Claude API key belongs to the
    // *instance*, not to any one project — there is no `{key}` to decide access
    // on, and scoping a token to a project would be a category error. Access is
    // the handler's [`crate::auth::extract::RequireAdmin`]: only an instance admin
    // may add, list, delete, or validate a credential, which is stricter than any
    // project role. The `id` in the delete/validate paths is a credential id, not
    // a project key, and must not be resolved as one — hence `Unscoped`, which
    // resolves nothing and stands aside for the handler's own guard.
    (Method::GET, "/api/v1/credentials", Scope::Unscoped),
    (Method::POST, "/api/v1/credentials", Scope::Unscoped),
    (Method::DELETE, "/api/v1/credentials/{id}", Scope::Unscoped),
    (
        Method::POST,
        "/api/v1/credentials/{id}/validate",
        Scope::Unscoped,
    ),
];

/// The scope declared for a route, or `None` if it has none — which is a bug.
pub(crate) fn scope_for(method: &Method, matched_path: &str) -> Option<Scope> {
    SCOPES
        .iter()
        .find(|(m, path, _)| m == method && *path == matched_path)
        .map(|(_, _, scope)| *scope)
}

/// Whether a route has been given a project scope.
///
/// Public so that `tests/project_access.rs` can enumerate the live OpenAPI
/// document and check it against this table by name, with a message that says
/// which route is missing.
#[must_use]
pub fn is_classified(method: &Method, matched_path: &str) -> bool {
    scope_for(method, matched_path).is_some()
}

/// Every route whose access is decided against a project, and the least project
/// role it demands.
///
/// The [`Scope::Unscoped`] and [`Scope::SelfFiltered`] routes are absent: they
/// have no project to decide on, so there is no role to report.
///
/// Public for the same reason as [`is_classified`], and it buys the same
/// property: `tests/project_access.rs` proves the instance-role **ceiling** holds
/// by walking *this table* rather than a hand-written list of routes to try. A
/// list in the test would go stale the moment somebody adds a route and forgets,
/// which is the exact failure the ceiling must not have.
#[must_use]
pub fn scoped_routes() -> Vec<(Method, &'static str, ProjectRole)> {
    SCOPES
        .iter()
        .filter_map(|(method, path, scope)| {
            scope.min_role().map(|role| (method.clone(), *path, role))
        })
        .collect()
}

/// Every method [`SCOPES`] declares for a path, for an `Allow` header.
///
/// Exact rather than approximate, because [`assert_scopes_match_routes`] proves
/// this table and the router's real route set are the same set.
fn allowed_methods(matched_path: &str) -> String {
    let mut allowed: Vec<&str> = Vec::new();
    for (method, path, _) in SCOPES {
        if *path != matched_path {
            continue;
        }
        allowed.push(method.as_str());
        // axum's `get()` also serves HEAD, so a GET route really does accept it.
        if *method == Method::GET {
            allowed.push("HEAD");
        }
    }
    allowed.join(", ")
}

/// What the layer makes of one request.
enum Classification {
    /// Classified. This is the scope to enforce.
    Scoped(Scope),
    /// The path is served, but not by this method.
    ///
    /// The router would answer 405 here, and
    /// [`assert_scopes_match_routes`] has already proved at startup that no
    /// handler exists for this pair — so the layer answers 405 itself rather
    /// than letting an unclassified `(method, path)` through to find out.
    NoSuchMethod,
    /// Nobody classified this route. A bug; refused loudly.
    Unclassified,
}

/// Which of the three a request is.
///
/// # The `HEAD` case
///
/// axum's `get()` answers `HEAD` from the same handler, so a `HEAD` request *is*
/// the `GET` route's read and has to be authorised as one. Looking it up as
/// itself would find nothing, and this layer would then have to guess — which is
/// exactly the hole `auth_gate_adversarial`'s
/// `a_head_request_cannot_slip_past_the_gate_on_a_get_route` pins for the
/// forced-reset gate: without this line, `HEAD` is an unguarded read oracle over
/// every readable route in Atlas.
fn classify(method: &Method, matched_path: &str) -> Classification {
    let lookup = if *method == Method::HEAD {
        &Method::GET
    } else {
        method
    };

    if let Some(scope) = scope_for(lookup, matched_path) {
        return Classification::Scoped(scope);
    }

    if SCOPES.iter().any(|(_, path, _)| *path == matched_path) {
        return Classification::NoSuchMethod;
    }

    Classification::Unclassified
}

/// Panics unless [`SCOPES`] and the router's real routes are **the same set**.
///
/// # Why this is a panic at startup and not a test
///
/// It is both — but the panic is the one that matters. It runs from
/// [`crate::api::router`], so an unclassified route means **the binary does not
/// boot** and every single integration test fails at its first line. There is no
/// way to run Atlas with a route nobody classified, which is a stronger promise
/// than any per-request check could make: you cannot deploy the mistake, so you
/// cannot be exposed by it.
///
/// It also earns the layer's [`Classification::NoSuchMethod`] arm. Because this
/// has run, a `(method, path)` that is *not* in [`SCOPES`] provably has no
/// handler, so answering 405 there is a fact rather than a hope.
///
/// # Both directions
///
/// - A route in the document but not in the table is the dangerous drift: a new
///   route nobody scoped.
/// - A row in the table but not in the document is stale, and matters because
///   [`allowed_methods`] reports the table as fact in an `Allow` header. It is
///   also the fingerprint of a typo'd path, which would otherwise leave the real
///   route unclassified *and* silently pass this check from the other side.
///
/// # Panics
///
/// If the two sets differ, naming every route that is in one and not the other.
pub fn assert_scopes_match_routes(openapi: &utoipa::openapi::OpenApi) {
    let mut documented: Vec<(Method, String)> = Vec::new();

    for (path, item) in &openapi.paths.paths {
        // Only the nest this layer wraps. `/healthz` and the Swagger UI are
        // mounted outside it and are not its business.
        if !path.starts_with(crate::api::API_V1_PREFIX) {
            continue;
        }
        for (method, operation) in [
            (Method::GET, &item.get),
            (Method::PUT, &item.put),
            (Method::POST, &item.post),
            (Method::DELETE, &item.delete),
            (Method::OPTIONS, &item.options),
            (Method::HEAD, &item.head),
            (Method::PATCH, &item.patch),
            (Method::TRACE, &item.trace),
        ] {
            if operation.is_some() {
                documented.push((method, path.clone()));
            }
        }
    }

    assert!(
        documented.len() > 20,
        "only {} routes found under {} — the OpenAPI document is not being read correctly, so \
         this check would pass vacuously",
        documented.len(),
        crate::api::API_V1_PREFIX
    );

    let unclassified: Vec<String> = documented
        .iter()
        .filter(|(method, path)| !is_classified(method, path))
        .map(|(method, path)| format!("{method} {path}"))
        .collect();

    assert!(
        unclassified.is_empty(),
        "{} route(s) under {} have no entry in auth::project_access::SCOPES:\n  {}\n\nEvery \
         route must state its project scope — including the ones that have none \
         (Scope::Unscoped). Atlas refuses to start rather than serve a route whose access rules \
         nobody wrote down.",
        unclassified.len(),
        crate::api::API_V1_PREFIX,
        unclassified.join("\n  ")
    );

    let stale: Vec<String> = SCOPES
        .iter()
        .filter(|(method, path, _)| !documented.iter().any(|(m, p)| m == method && p == *path))
        .map(|(method, path, _)| format!("{method} {path}"))
        .collect();

    assert!(
        stale.is_empty(),
        "{} entr(ies) in auth::project_access::SCOPES name a route that does not exist:\n  \
         {}\n\nEither the route was removed and its entry should go too, or the path is a typo — \
         in which case the real route is unclassified and this table is lying about it.",
        stale.len(),
        stale.join("\n  ")
    );
}

/// The paths served **outside** the gated `/api/v1` nest, and why.
///
/// `/healthz` names no project, reads nothing a session could protect, and has to
/// answer before anybody has signed in — it is a liveness probe for a load
/// balancer. That is the entire list, and [`assert_no_route_escapes_the_gate`]
/// keeps it that way.
const UNGATED_PATHS: &[&str] = &["/healthz"];

/// Panics if any route is served outside the three `/api/v1` layers.
///
/// # The hole this closes
///
/// [`assert_scopes_match_routes`] skips every path that does not start with
/// `/api/v1`, and it must — `/healthz` has no project scope to state. But that
/// makes "outside the nest" the one place a route can be added where **nothing**
/// notices: no `verify_origin`, no `authenticate`, no `authorise`, and no entry
/// in [`SCOPES`] demanded of it. It would not fail closed and it would not fail
/// loudly; it would simply be served, to everybody, unauthenticated.
///
/// That is not hypothetical. Phase 8's WebSocket board sync and Phase 12's GitHub
/// webhook receiver are both routes somebody will reach for a top-level mount for,
/// and `crate::api::router` already has `.routes(routes!(healthz))` sitting at the
/// top level as the pattern to copy.
///
/// So the nest's boundary is asserted rather than assumed: a new top-level route
/// stops the binary from booting until somebody either moves it under `/api/v1`
/// or argues it onto [`UNGATED_PATHS`] in review. Same deny-by-default trade as
/// [`SCOPES`] itself, one level out.
///
/// # Panics
///
/// If any documented path outside `/api/v1` is not in [`UNGATED_PATHS`].
pub fn assert_no_route_escapes_the_gate(openapi: &utoipa::openapi::OpenApi) {
    let escaped: Vec<&String> = openapi
        .paths
        .paths
        .keys()
        .filter(|path| !path.starts_with(crate::api::API_V1_PREFIX))
        .filter(|path| !UNGATED_PATHS.contains(&path.as_str()))
        .collect();

    assert!(
        escaped.is_empty(),
        "{} route(s) are served outside the {} nest:\n  {}\n\nEverything that authorises a \
         request in Atlas is a layer over that nest: the CSRF origin check, the session lookup, \
         the forced-reset gate, and this module's project-access decision. A route mounted \
         outside it has none of them and is not required to appear in \
         auth::project_access::SCOPES either — so it is served to anybody who asks, and nothing \
         anywhere says so.\n\nMount it under {} instead. If it genuinely must be public — a \
         liveness probe, say — add it to auth::project_access::UNGATED_PATHS, where the next \
         person reviewing this can see the decision was made on purpose.",
        escaped.len(),
        crate::api::API_V1_PREFIX,
        escaped
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
        crate::api::API_V1_PREFIX,
    );
}

/// What a route's path parameter pointed at.
enum Target {
    /// Nothing. The card/comment/tag/status id names no row.
    Missing,
    /// A project — either directly, or the one owning the thing named.
    Owned(Box<Project>),
    /// A global tag: real, but owned by no project.
    Global,
}

/// Resolves a route's path parameter to the project access is decided on.
async fn resolve_target(db: &Db, scope: Scope, value: &str) -> AppResult<Target> {
    let project_id: Option<String> = match scope {
        // Nothing to resolve; handled before this is ever called.
        Scope::Unscoped | Scope::SelfFiltered => return Ok(Target::Missing),

        Scope::Project(_) => {
            return Ok(match project::find_by_key(db, value).await? {
                Some(project) => Target::Owned(Box::new(project)),
                None => Target::Missing,
            });
        }

        // Retired keys resolve too, to the project the card lives in **now**.
        // `GET /cards/{key}` answers 301 for a moved key rather than 404, and a
        // layer that could not see past `cards.key` would 404 it before the
        // handler ever ran — silently breaking every bookmark and commit message
        // that `card_key_history` exists to keep working. Deciding access on the
        // current project is also the right answer: if you cannot see where the
        // card went, you should not learn that it went.
        Scope::Card(_) => {
            let key = value.to_ascii_uppercase();
            sqlx::query_scalar(
                "SELECT COALESCE( \
                     (SELECT project_id FROM cards WHERE key = ?), \
                     (SELECT c.project_id FROM cards c \
                        JOIN card_key_history h ON h.card_id = c.id \
                       WHERE h.old_key = ?) \
                 )",
            )
            .bind(&key)
            .bind(&key)
            .fetch_one(db.reader())
            .await?
        }

        Scope::Comment(_) => {
            sqlx::query_scalar(
                "SELECT c.project_id FROM cards c \
                   JOIN comments m ON m.card_id = c.id \
                  WHERE m.id = ?",
            )
            .bind(value)
            .fetch_optional(db.reader())
            .await?
        }

        // `tags.project_id` is nullable: NULL is a global tag, usable from every
        // project. So "no row" and "a real tag owned by nobody" are different
        // answers, and `fetch_optional` on a nullable column returns
        // Option<Option<_>> to tell them apart.
        Scope::Tag(_) => {
            let found: Option<Option<String>> =
                sqlx::query_scalar("SELECT project_id FROM tags WHERE id = ?")
                    .bind(value)
                    .fetch_optional(db.reader())
                    .await?;
            match found {
                None => return Ok(Target::Missing),
                Some(None) => return Ok(Target::Global),
                Some(Some(id)) => Some(id),
            }
        }

        Scope::Config(table, _) => {
            sqlx::query_scalar(table.project_query())
                .bind(value)
                .fetch_optional(db.reader())
                .await?
        }

        Scope::Workflow(_) => {
            sqlx::query_scalar("SELECT project_id FROM workflows WHERE id = ?")
                .bind(value)
                .fetch_optional(db.reader())
                .await?
        }

        // A saved board is owned directly by a project.
        Scope::Board(_) => {
            sqlx::query_scalar("SELECT project_id FROM boards WHERE id = ?")
                .bind(value)
                .fetch_optional(db.reader())
                .await?
        }

        // A transition is owned by a workflow, which is owned by a project.
        Scope::Transition(_) => {
            sqlx::query_scalar(
                "SELECT w.project_id FROM workflows w \
                   JOIN transitions t ON t.workflow_id = w.id \
                  WHERE t.id = ?",
            )
            .bind(value)
            .fetch_optional(db.reader())
            .await?
        }
    };

    let Some(project_id) = project_id else {
        return Ok(Target::Missing);
    };

    Ok(match project::find_by_id(db, &project_id).await? {
        Some(project) => Target::Owned(Box::new(project)),
        // A row pointing at a project that does not exist. Every one of these
        // foreign keys cascades, so this is unreachable while the database is
        // consistent — and a 404 is the right answer to an inconsistent one.
        None => Target::Missing,
    })
}

/// Refuses every project-scoped request the caller is not entitled to make.
///
/// Innermost of the three `/api/v1` layers, so it runs after
/// [`crate::auth::middleware::authenticate`] has put a [`CurrentUser`] in the
/// extensions and after the forced-reset gate has had its say.
pub async fn authorise(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(matched) = request.extensions().get::<MatchedPath>().cloned() else {
        // Cannot happen: axum inserts this when it matches a route, and this
        // layer only ever wraps matched routes. Refuse rather than guess.
        tracing::error!(
            path = %request.uri().path(),
            "no MatchedPath on a request inside the /api/v1 nest — refusing it, because a \
             project-access decision cannot be made without knowing which route matched"
        );
        return AppError::internal(anyhow::anyhow!("no MatchedPath in the /api/v1 nest"))
            .into_response();
    };

    let method = request.method().clone();

    let scope = match classify(&method, matched.as_str()) {
        Classification::Scoped(scope) => scope,

        // The router would answer 405 here anyway; the layer answers it instead
        // so that an unclassified `(method, path)` can never reach a handler
        // even if one were somehow added without a `routes!` declaration. The
        // `Allow` header is exact — `assert_scopes_match_routes` proved this
        // table is the route set — so RFC 9110's "a 405 MUST generate an Allow
        // header" is honoured, and the answer is byte-identical in shape to the
        // one axum's own method router would have produced.
        Classification::NoSuchMethod => {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, allowed_methods(matched.as_str()))
                .body(Body::empty())
                .unwrap_or_else(|err| {
                    AppError::internal(anyhow::anyhow!("failed to build a 405: {err}"))
                        .into_response()
                });
        }

        // Unreachable in a binary that booted: `assert_scopes_match_routes` runs
        // from `api::router` and panics on exactly this. Kept anyway, because
        // "unreachable" is a claim about today's wiring and this is the last
        // thing standing between an unscoped route and every project in the
        // database. 500 rather than 403: an unclassified route is a defect in
        // Atlas, not a statement about the caller, and a 403 would blend into
        // ordinary authorisation traffic while a 500 is the one status everybody
        // has an alarm on. Either way it fails closed.
        Classification::Unclassified => {
            tracing::error!(
                %method,
                route = %matched.as_str(),
                "route has no project-access classification and is therefore refused. Add it to \
                 auth::project_access::SCOPES — every route under /api/v1 must state its project \
                 scope, including the ones that do not have one (Scope::Unscoped)."
            );
            return AppError::internal(anyhow::anyhow!(
                "route {method} {} has no project-access classification",
                matched.as_str()
            ))
            .into_response();
        }
    };

    // The instance role is the whole answer here; the handler's own guard applies
    // it. `POST /auth/login` reaches its handler through this arm, which is why
    // the session check below sits *after* it rather than at the top.
    if scope == Scope::Unscoped {
        return next.run(request).await;
    }

    let Some(current) = request.extensions().get::<CurrentUser>().cloned() else {
        return AppError::Unauthorized.into_response();
    };

    // The handler filters its own result set; there is no single project to
    // decide on. A list must never 403.
    if scope == Scope::SelfFiltered {
        return next.run(request).await;
    }

    match check(&state.db, scope, &mut request, &current).await {
        Ok(()) => next.run(request).await,
        Err(err) => err.into_response(),
    }
}

/// The decision for one project-scoped request.
async fn check(
    db: &Db,
    scope: Scope,
    request: &mut Request,
    current: &CurrentUser,
) -> AppResult<()> {
    let (Some(name), Some(min)) = (scope.param(), scope.min_role()) else {
        // Unreachable: both are Some for every scope that reaches here.
        return Err(AppError::internal(anyhow::anyhow!(
            "scope {scope:?} reached the resolver with nothing to resolve"
        )));
    };

    // `RawPathParams` rather than parsing the URI by hand: it reads the very same
    // `UrlParams` extension the handler's `Path` extractor reads, already
    // percent-decoded. Anything else would risk the layer checking access to one
    // project while the handler operates on another — `%41TLAS` and `ATLAS` are
    // the same project to axum, and must be to this layer too.
    let params = request
        .extract_parts::<RawPathParams>()
        .await
        .map_err(|_| {
            AppError::internal(anyhow::anyhow!(
                "no path parameters on a {scope:?} route, which must have a {{{name}}}"
            ))
        })?;

    let Some(value) = params
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
    else {
        tracing::error!(
            scope = ?scope,
            expected = name,
            "route is classified with a scope whose path parameter it does not have. Either the \
             classification in auth::project_access::SCOPES or the route's parameter name is \
             wrong."
        );
        return Err(AppError::internal(anyhow::anyhow!(
            "a {scope:?} route has no {{{name}}} parameter"
        )));
    };

    match resolve_target(db, scope, &value).await? {
        // 404, not 403. See `member::require`.
        Target::Missing => Err(AppError::NotFound),

        // A global tag belongs to no project, so no project's owner has any
        // authority over it — and it is usable from every project, so letting one
        // project's member rename or delete it would let them reach into every
        // other project's boards. Instance-wide scope, instance-wide authority:
        // admins only. 403 rather than 404 because the tag's existence is not a
        // secret; every project can already see it in its own tag list.
        Target::Global => {
            if current.has_role(Role::Admin) {
                Ok(())
            } else {
                Err(AppError::Forbidden)
            }
        }

        Target::Owned(project) => member::require(db, &project, &current.user, min)
            .await
            .map(|_role| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::openapi::path::{HttpMethod, Operation, PathItem};
    use utoipa::openapi::{OpenApiBuilder, PathsBuilder};

    /// An OpenAPI document serving exactly the paths named.
    ///
    /// [`assert_no_route_escapes_the_gate`] reads nothing but the path keys, so
    /// a bare `GET` operation per path is a faithful stand-in for the real
    /// document and needs no handler to exist.
    fn document(paths: &[&str]) -> utoipa::openapi::OpenApi {
        let mut builder = PathsBuilder::new();
        for path in paths {
            builder = builder.path(*path, PathItem::new(HttpMethod::Get, Operation::new()));
        }
        OpenApiBuilder::new().paths(builder.build()).build()
    }

    #[test]
    fn the_health_check_is_allowed_to_live_outside_the_gate() {
        // It names no project, reads nothing a session protects, and must answer
        // before anybody signs in. If this ever panics, `UNGATED_PATHS` and the
        // router have drifted and `/healthz` is about to stop the binary booting.
        assert_no_route_escapes_the_gate(&document(&["/healthz"]));

        // ...and so is everything under the nest, which is the normal case.
        assert_no_route_escapes_the_gate(&document(&[
            "/healthz",
            "/api/v1/projects",
            "/api/v1/projects/{key}/cards",
        ]));
    }

    #[test]
    #[should_panic(expected = "served outside the")]
    fn a_route_mounted_outside_the_gate_stops_the_binary_from_booting() {
        // The hole this closes: `assert_scopes_match_routes` only looks under
        // `/api/v1`, so a top-level mount is the one place a route can be added
        // that nothing checks — no CSRF, no session, no project-access decision,
        // and no entry demanded in SCOPES. It would be served to anybody.
        //
        // Phase 8's board WebSocket is exactly the route somebody reaches for a
        // top-level mount for, so this is the shape it must fail in.
        assert_no_route_escapes_the_gate(&document(&["/healthz", "/ws/boards/{key}"]));
    }

    #[test]
    fn every_scope_that_resolves_a_project_knows_how_to_find_one() {
        // `param` and `min_role` must agree about which scopes resolve: `check`
        // treats a disagreement as an internal error, and this is what stops that
        // from being reachable.
        let scopes = [
            Scope::Unscoped,
            Scope::SelfFiltered,
            Scope::Project(ProjectRole::Viewer),
            Scope::Card(ProjectRole::Member),
            Scope::Comment(ProjectRole::Member),
            Scope::Tag(ProjectRole::Member),
            Scope::Config(ConfigTable::Statuses, ProjectRole::Owner),
        ];

        for scope in scopes {
            assert_eq!(
                scope.param().is_some(),
                scope.min_role().is_some(),
                "{scope:?} disagrees with itself about whether it resolves a project"
            );
        }
    }

    #[test]
    fn an_unclassified_route_resolves_to_nothing() {
        // The deny-by-default property, at the lookup. A route nobody classified
        // has no scope, and `authorise` turns that into a 500.
        assert!(scope_for(&Method::GET, "/api/v1/phase-8/boards").is_none());
        assert!(!is_classified(&Method::GET, "/api/v1/phase-8/boards"));

        // ...and the classification is per-method, not per-path: a new verb on an
        // existing path is a new route and must be classified afresh.
        assert!(is_classified(&Method::GET, "/api/v1/projects/{key}"));
        assert!(!is_classified(&Method::PUT, "/api/v1/projects/{key}"));
    }

    #[test]
    fn the_table_is_keyed_on_route_templates_not_concrete_paths() {
        // The layer matches `MatchedPath`, which is always a template. An entry
        // written as a concrete path would never match anything and would leave
        // its route refused — so make the shape explicit.
        for (_, path, _) in SCOPES {
            assert!(
                path.starts_with("/api/v1/"),
                "{path} is missing the nest prefix; MatchedPath includes it"
            );
            assert!(
                !path.contains(':'),
                "{path} uses axum 0.7 `:param` syntax; 0.8 is `{{param}}`"
            );
        }
    }

    #[test]
    fn no_route_is_classified_twice() {
        // `scope_for` takes the first match, so a duplicate would silently shadow
        // whichever entry came second — including a stricter one.
        let mut seen: Vec<(&Method, &str)> = Vec::new();
        for (method, path, _) in SCOPES {
            assert!(
                !seen.contains(&(method, path)),
                "{method} {path} is classified twice"
            );
            seen.push((method, path));
        }
    }

    #[test]
    fn reads_are_never_stricter_than_the_writes_beside_them() {
        // A GET that demanded Owner while its sibling POST demanded Member would
        // be a capability matrix nobody could explain. Pin the direction.
        for (method, path, scope) in SCOPES {
            if *method != Method::GET {
                continue;
            }
            if let Some(role) = scope.min_role() {
                assert_eq!(
                    role,
                    ProjectRole::Viewer,
                    "{method} {path} is a read but demands {role}"
                );
            }
        }
    }
}
