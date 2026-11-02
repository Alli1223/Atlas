# AQL — the Atlas Query Language

AQL is Jira's JQL, kept compatible where it can be. Search, saved filters, boards, quick
filters, dashboards, the AQL reports and automation conditions all use it, and every query is
filtered to the cards the person running it can see.

```
project = JOB AND status != Rejected AND due <= 7d ORDER BY due ASC
```

## Fields

| Field | Aliases | Notes |
| --- | --- | --- |
| `project` | | project key |
| `type` | `issuetype`, `cardtype` | card type name |
| `status` | | status name |
| `statusCategory` | | `todo`, `in_progress`, `done` |
| `priority` | | name; `>`/`<` compare by rank (`priority > Low`) |
| `assignee`, `reporter`, `creator` | | username |
| `resolution` | | name; `resolution IS EMPTY` = unresolved |
| `parent` | | parent card key |
| `created`, `updated`, `resolved` | `resolutiondate` | instants |
| `due` | `duedate` | date |
| `started` | `startdate` | date |
| `summary`, `description`, `text` | | use `~` (contains); `text` searches both |
| `labels` | `label`, `tag`, `tags` | tag name |
| `key` | `id`, `card` | card key |
| `estimate` | `estimation` | number |
| `milestone` | `fixversion`, `version` | milestone name |
| `component` | `components` | component name |

## Operators

`=` `!=` `>` `>=` `<` `<=` `IN (…)` `NOT IN (…)` `~` `!~` `IS EMPTY` `IS NOT EMPTY`
(`NULL` is accepted for `EMPTY`), and the history operators `WAS`, `WAS NOT`, `WAS IN`,
`WAS NOT IN` and `CHANGED`:

```
status WAS "In Review"
status CHANGED
```

Combine with `AND`, `OR`, `NOT` and parentheses. `ORDER BY field [ASC|DESC], …` sorts.

## Dates

Absolute (`2026-11-01`), relative (`-7d`, `+1w`, `-2h`, `-30m`, units `w d h m`), or a function.
A bare number is never a date — `-7` is refused rather than guessed.

## Functions

| Function | Meaning |
| --- | --- |
| `currentUser()` | the person running the query |
| `now()` | this instant |
| `startOfDay()` `startOfWeek()` `startOfMonth()` `startOfYear()` | and the matching `endOf…()`; each takes an offset, e.g. `startOfWeek(-1w)` |
| `membersOf("project")` | the project's members |
| `watchedCards()` | cards you watch |
| `cardHistory()` | cards you have ever changed |
| `linkedCards("KEY-1")` | cards linked to that card |
| `customField("Salary") > 90000` | a custom field, by name |

## Examples

```
assignee = currentUser() AND resolution IS EMPTY ORDER BY priority DESC
project = JOB AND status = Applied AND updated <= -14d
labels IN (remote, dream) AND due < endOfWeek()
project = ART AND component = Characters AND status CHANGED
customField("Interest") = High
```
