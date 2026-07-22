import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** A card. Mirrors `crate::domain::card::CardDto`. */
export type Card = components['schemas']['CardDto']
/** A comment. Mirrors `crate::domain::comment::Comment`. */
export type Comment = components['schemas']['Comment']
/** A changelog row. Mirrors `crate::domain::history::HistoryEntry`. */
export type HistoryEntry = components['schemas']['HistoryEntry']
/** A move a card may legally make right now. Mirrors `AvailableTransition`. */
export type AvailableTransition = components['schemas']['AvailableTransition']
/** A workflow status (board column). Mirrors `crate::domain::config::Status`. */
export type Status = components['schemas']['Status']
/** The three status buckets. */
export type StatusCategory = components['schemas']['StatusCategory']
/** A priority. Mirrors `crate::domain::config::Priority`. */
export type Priority = components['schemas']['Priority']
/** A resolution. Mirrors `crate::domain::config::Resolution`. */
export type Resolution = components['schemas']['Resolution']
/** A card type. Mirrors `crate::domain::config::CardType`. */
export type CardType = components['schemas']['CardType']
/** A project member. Mirrors `crate::domain::member::ProjectMemberDto`. */
export type ProjectMember = components['schemas']['ProjectMemberDto']

/**
 * The project key a card key belongs to.
 *
 * A card key is `<PROJECT>-<counter>` and the project key is uppercase with no hyphen (see
 * `crate::domain::project`), so everything before the final `-` is the project. Derived
 * rather than fetched because the config endpoints are keyed by project *key*, and a card
 * only carries its project *id* — asking the server to translate would be a round trip for
 * something the key already states.
 */
export function projectKeyOf(cardKey: string): string {
  const cut = cardKey.lastIndexOf('-')
  return cut === -1 ? cardKey : cardKey.slice(0, cut)
}

/** One card. A retired key 301s to the current one; openapi-fetch follows it transparently. */
export async function fetchCard(key: string): Promise<Card> {
  return unwrap(await api.GET('/api/v1/cards/{key}', { params: { path: { key } } }))
}

/** The fields a PATCH may carry. Mirrors the settable half of `UpdateCardRequest`. */
export interface CardPatch {
  summary?: string
  /** `null` clears the description, a string sets it, absent leaves it. */
  description?: string | null
  statusId?: string
  /** `null` clears, a string sets, absent leaves. */
  priorityId?: string | null
  assigneeId?: string | null
  reporterId?: string | null
  resolutionId?: string | null
  dueDate?: string | null
  startDate?: string | null
  estimate?: number | null
  typeId?: string
  archived?: boolean
}

/** Edits a card. The server diffs every field into `card_history` in the same transaction. */
export async function patchCard(key: string, patch: CardPatch): Promise<Card> {
  return unwrap(
    await api.PATCH('/api/v1/cards/{key}', { params: { path: { key } }, body: patch }),
  )
}

/** Every comment on a card, oldest first. */
export async function fetchComments(key: string): Promise<Comment[]> {
  return unwrap(await api.GET('/api/v1/cards/{key}/comments', { params: { path: { key } } }))
}

/** Posts a comment (markdown source). */
export async function postComment(key: string, body: string): Promise<Comment> {
  return unwrap(
    await api.POST('/api/v1/cards/{key}/comments', { params: { path: { key } }, body: { body } }),
  )
}

/** Edits a comment. Authors only — the backend enforces it. */
export async function patchComment(id: string, body: string): Promise<Comment> {
  return unwrap(
    await api.PATCH('/api/v1/comments/{id}', { params: { path: { id } }, body: { body } }),
  )
}

/** Deletes a comment. The author, or an admin. */
export async function deleteComment(id: string): Promise<void> {
  unwrap(await api.DELETE('/api/v1/comments/{id}', { params: { path: { id } } }))
}

/** A card's changelog, oldest first. */
export async function fetchHistory(key: string): Promise<HistoryEntry[]> {
  return unwrap(await api.GET('/api/v1/cards/{key}/history', { params: { path: { key } } }))
}

/** A card's children — the nested board, scoped by parent. */
export async function fetchChildren(key: string): Promise<Card[]> {
  return unwrap(await api.GET('/api/v1/cards/{key}/children', { params: { path: { key } } }))
}

/**
 * The transitions a card may take right now.
 *
 * The backend has already evaluated every transition's *conditions* and dropped the ones
 * that fail, so this list is exactly the set of buttons to show — never a move the user
 * cannot make. (Validators are different: those transitions are shown and rejected on
 * attempt, with the reason surfaced.)
 */
export async function fetchTransitions(key: string): Promise<AvailableTransition[]> {
  return unwrap(await api.GET('/api/v1/cards/{key}/transitions', { params: { path: { key } } }))
}

/** Fields a transition screen may collect before the move runs. */
export interface ExecuteTransitionInput {
  comment?: string
  resolutionId?: string | null
  assigneeId?: string | null
}

/** Takes a named transition: validators, the status change, then post-functions. */
export async function executeTransition(
  key: string,
  transitionId: string,
  input: ExecuteTransitionInput = {},
): Promise<Card> {
  return unwrap(
    await api.POST('/api/v1/cards/{key}/transitions/{id}', {
      params: { path: { key, id: transitionId } },
      body: input,
    }),
  )
}

/** Every status of a project, in board order. */
export async function fetchStatuses(projectKey: string): Promise<Status[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/statuses', { params: { path: { key: projectKey } } }),
  )
}

/** Every priority of a project, most urgent first. */
export async function fetchPriorities(projectKey: string): Promise<Priority[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/priorities', { params: { path: { key: projectKey } } }),
  )
}

/** Every resolution of a project. */
export async function fetchResolutions(projectKey: string): Promise<Resolution[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/resolutions', { params: { path: { key: projectKey } } }),
  )
}

/** Every card type of a project. */
export async function fetchCardTypes(projectKey: string): Promise<CardType[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/card-types', { params: { path: { key: projectKey } } }),
  )
}

/** A project's members — the assignee and reporter candidates. */
export async function fetchMembers(projectKey: string): Promise<ProjectMember[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/members', { params: { path: { key: projectKey } } }),
  )
}

/**
 * A page of a project's cards, at any depth.
 *
 * Used only to resolve a card's *parent* to a key and summary for the sidebar link — a card
 * carries its `parentId` (a UUID) but there is no fetch-card-by-id endpoint, and the parent's
 * key is what a link needs. Capped at 200 and fetched lazily (only when a parent exists), so
 * a childless card never pays for it.
 */
export async function fetchProjectCards(projectKey: string): Promise<Card[]> {
  const page = unwrap(
    await api.GET('/api/v1/projects/{key}/cards', {
      params: { path: { key: projectKey }, query: { limit: 200 } },
    }),
  )
  return page.cards
}
