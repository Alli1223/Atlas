import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** The whole board: columns, and optionally swimlanes. Mirrors `crate::domain::board::BoardData`. */
export type BoardData = components['schemas']['BoardData']
/** One column: a status and its cards, in rank order. */
export type BoardColumn = components['schemas']['BoardColumn']
/** One card as the board renders it. */
export type BoardCard = components['schemas']['BoardCard']
/** A column's status header. */
export type BoardColumnStatus = components['schemas']['BoardColumnStatus']
/** A labelled partition of the board's cards. */
export type BoardSwimlane = components['schemas']['BoardSwimlane']
/** A card's children summarised by status category — the mini-map. */
export type ChildRollup = components['schemas']['ChildRollup']
/** A saved board configuration. Mirrors `crate::domain::board::Board`. */
export type SavedBoard = components['schemas']['Board']

/** A move a card may legally make right now. Mirrors `crate::domain::workflow::AvailableTransition`. */
export type AvailableTransition = components['schemas']['AvailableTransition']

/** A card, as the card endpoints describe it. Mirrors `crate::domain::card::CardDto`. */
export type CardDto = components['schemas']['CardDto']

/** A project status. Mirrors `crate::domain::config::Status`. */
export type Status = components['schemas']['Status']
/** A card type. Mirrors `crate::domain::config::CardType`. */
export type CardType = components['schemas']['CardType']
/** A priority. Mirrors `crate::domain::config::Priority`. */
export type Priority = components['schemas']['Priority']
/** A user. Mirrors `crate::auth::user::UserDto`. */
export type UserDto = components['schemas']['UserDto']

/** Which slice of the board to fetch, and how to group it. */
export interface BoardParams {
  /** A parent card key to render the children of — the nested board. Omit for the top level. */
  parent?: string
  /** An AQL quick filter, ANDed onto the board's scope. */
  aql?: string
  /** Row grouping. */
  swimlane?: 'none' | 'assignee' | 'parent'
}

/**
 * The board data for a project.
 *
 * `columns` is always the full ungrouped board; `swimlanes` is present only when a grouping
 * was requested and partitions the *same* cards. The client renders one or the other.
 */
export async function fetchBoard(projectKey: string, params: BoardParams = {}): Promise<BoardData> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/board', {
      params: {
        path: { key: projectKey },
        query: {
          ...(params.parent !== undefined && { parent: params.parent }),
          ...(params.aql !== undefined && params.aql !== '' && { aql: params.aql }),
          ...(params.swimlane !== undefined && { swimlane: params.swimlane }),
        },
      },
    }),
  )
}

/**
 * The transitions a card may take **right now** — conditions already evaluated server-side,
 * so this only ever lists legal moves. A transition with a `null` id is a permissive
 * "move to X" the client takes through the move endpoint; a non-null id is a real workflow
 * transition taken through the transition-execute endpoint.
 */
export async function fetchCardTransitions(cardKey: string): Promise<AvailableTransition[]> {
  return unwrap(
    await api.GET('/api/v1/cards/{key}/transitions', { params: { path: { key: cardKey } } }),
  )
}

/**
 * Executes a workflow transition on a card: validators, the status change, then
 * post-functions, all in one server-side transaction. A rejected validator is a 422, an
 * illegal/hidden transition a 409 — both surface as `ApiError` for the board to roll back on.
 */
export async function executeTransition(
  cardKey: string,
  transitionId: string,
): Promise<CardDto> {
  return unwrap(
    await api.POST('/api/v1/cards/{key}/transitions/{id}', {
      params: { path: { key: cardKey, id: transitionId } },
      body: {},
    }),
  )
}

/** The neighbours a card is dropped between, for rank positioning. */
export interface MoveCardBody {
  /** The target column, when the drop changes status. Omit to reorder within the column. */
  statusId?: string
  /** The card immediately above the drop point, or omit for the top. */
  previousCardId?: string
  /** The card immediately below the drop point, or omit for the bottom. */
  nextCardId?: string
}

/**
 * Moves a card to a status and/or a rank position. This is the rank-aware path, used for
 * same-column reordering and for cross-column moves under a permissive workflow (where
 * there is no transition to execute).
 */
export async function moveCard(cardKey: string, body: MoveCardBody): Promise<CardDto> {
  return unwrap(
    await api.POST('/api/v1/cards/{key}/move', {
      params: { path: { key: cardKey } },
      body: {
        ...(body.statusId !== undefined && { statusId: body.statusId }),
        ...(body.previousCardId !== undefined && { previousCardId: body.previousCardId }),
        ...(body.nextCardId !== undefined && { nextCardId: body.nextCardId }),
      },
    }),
  )
}

/** A project's statuses, in board (position) order. */
export async function fetchStatuses(projectKey: string): Promise<Status[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/statuses', { params: { path: { key: projectKey } } }),
  )
}

/** A project's card types. */
export async function fetchCardTypes(projectKey: string): Promise<CardType[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/card-types', { params: { path: { key: projectKey } } }),
  )
}

/** A project's priorities, most urgent first. */
export async function fetchPriorities(projectKey: string): Promise<Priority[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/priorities', { params: { path: { key: projectKey } } }),
  )
}

/** Every user, for resolving assignee avatars. */
export async function fetchUsers(): Promise<UserDto[]> {
  return unwrap(await api.GET('/api/v1/users'))
}

/** One card by key — used for the nested-board breadcrumb (the parent's summary). */
export async function fetchCard(cardKey: string): Promise<CardDto> {
  return unwrap(await api.GET('/api/v1/cards/{key}', { params: { path: { key: cardKey } } }))
}

/** A project's saved boards. */
export async function fetchSavedBoards(projectKey: string): Promise<SavedBoard[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/boards', { params: { path: { key: projectKey } } }),
  )
}
