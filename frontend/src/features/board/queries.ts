import {
  queryOptions,
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'

import { ApiError } from '@/lib/api'

import * as boardApi from './api'
import type { BoardCard, BoardData, BoardParams } from './api'
import { applyMove } from './applyMove'
import { toast } from './toast'

/**
 * Query keys for everything board.
 *
 * The board data key carries the *params* (parent, filter, swimlane) because those change
 * which cards come back — two different filters are two different cached boards, and the
 * optimistic move must target the exact one on screen.
 */
export const boardKeys = {
  all: ['board'] as const,
  data: (projectKey: string, params: BoardParams) =>
    [...boardKeys.all, 'data', projectKey, params] as const,
  references: (projectKey: string) => [...boardKeys.all, 'references', projectKey] as const,
  saved: (projectKey: string) => [...boardKeys.all, 'saved', projectKey] as const,
}

/** The board data for a project and a set of view params. */
export function boardQueryOptions(projectKey: string, params: BoardParams) {
  return queryOptions({
    queryKey: boardKeys.data(projectKey, params),
    queryFn: () => boardApi.fetchBoard(projectKey, params),
    // A drop pushes the true state; between drops the board is stable. Short enough that a
    // parallel edit elsewhere reconciles quickly, long enough not to thrash on navigation.
    staleTime: 10_000,
  })
}

/** The board data for a project and a set of view params. */
export function useBoard(projectKey: string, params: BoardParams) {
  return useQuery(boardQueryOptions(projectKey, params))
}

/** One card by key. Used for the nested-board breadcrumb. */
export function useCard(cardKey: string | undefined) {
  return useQuery({
    queryKey: [...boardKeys.all, 'card', cardKey],
    queryFn: () => boardApi.fetchCard(cardKey!),
    enabled: cardKey !== undefined,
  })
}

/**
 * Several cards by key at once — the ancestor cards of a nested board, for the breadcrumb.
 *
 * One query per key (parallelised by `useQueries`), each sharing the same cache entry as
 * [`useCard`], so an ancestor already fetched as a `parent` costs nothing. The breadcrumb is
 * at most a few levels deep (the hierarchy depth cap is 5), so this never fans out widely.
 * Returns a `key → summary` map; a still-loading or missing card simply is not in it, and the
 * breadcrumb falls back to showing the raw key.
 */
export function useCardSummaries(keys: string[]): Map<string, string> {
  const results = useQueries({
    queries: keys.map((key) => ({
      queryKey: [...boardKeys.all, 'card', key] as const,
      queryFn: () => boardApi.fetchCard(key),
      staleTime: 30_000,
    })),
  })
  const map = new Map<string, string>()
  results.forEach((result, index) => {
    if (result.data) map.set(keys[index]!, result.data.summary)
  })
  return map
}

/** A project's saved boards. */
export function useSavedBoards(projectKey: string) {
  return useQuery({
    queryKey: boardKeys.saved(projectKey),
    queryFn: () => boardApi.fetchSavedBoards(projectKey),
  })
}

/**
 * The reference data a board card needs to render: card types (icons), priorities (icons),
 * and users (assignee avatars), each as a lookup by id.
 *
 * One combined hook rather than three at every call site. These rarely change, so they are
 * cached long — a card type is not renamed mid-drag.
 */
export function useBoardReferences(projectKey: string) {
  const cardTypes = useQuery({
    queryKey: [...boardKeys.references(projectKey), 'card-types'],
    queryFn: () => boardApi.fetchCardTypes(projectKey),
    staleTime: 5 * 60_000,
  })
  const priorities = useQuery({
    queryKey: [...boardKeys.references(projectKey), 'priorities'],
    queryFn: () => boardApi.fetchPriorities(projectKey),
    staleTime: 5 * 60_000,
  })
  const users = useQuery({
    queryKey: [...boardKeys.all, 'users'],
    queryFn: boardApi.fetchUsers,
    staleTime: 5 * 60_000,
  })

  const cardTypeById = new Map((cardTypes.data ?? []).map((t) => [t.id, t]))
  const priorityById = new Map((priorities.data ?? []).map((p) => [p.id, p]))
  const userById = new Map((users.data ?? []).map((u) => [u.id, u]))

  return { cardTypeById, priorityById, userById }
}

/**
 * A move the board could not carry out because the workflow does not allow it — distinct
 * from a network or server error so the snap-back toast can explain *which* rule blocked it.
 */
export class MoveError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'MoveError'
  }
}

/** A resolved drop, everything the mutation needs to commit and to explain a failure. */
export interface CardMove {
  card: BoardCard
  toStatusId: string
  toIndex: number
  /** A pure reorder within the same column — no status change, no workflow check. */
  sameColumn: boolean
  previousCardId?: string
  nextCardId?: string
  /** Human status names, for the snap-back message. */
  fromStatusName: string
  toStatusName: string
}

/**
 * Moves a card, optimistically, and rolls back if the server rejects it.
 *
 * The subtle part is that a cross-column drop is a **workflow transition**, not a raw status
 * set. So `mutationFn`:
 *
 *  1. A same-column drop is a pure rank reorder — the move endpoint, no workflow.
 *  2. A cross-column drop looks up the card's *legal* transitions (conditions already
 *     evaluated server-side). No transition reaching the target column → the drop is
 *     illegal; throw a [`MoveError`] so it snaps back with a reason.
 *  3. A legal transition with an id is a real workflow move (validators + post-functions):
 *     the transition-execute endpoint. A `null` id is a permissive move: the rank-aware
 *     move endpoint, which also auto-sets resolution on a drop into a done column.
 *
 * The four rules that make optimism correct: **cancelQueries first** (an in-flight refetch
 * resolving after the optimistic write would visibly snap the card back), **snapshot before
 * mutating**, **roll back from the snapshot in onError**, **invalidate in onSettled** (so a
 * rollback *and* a success both reconcile against the server's true rank order).
 */
export function useMoveCard(projectKey: string, params: BoardParams) {
  const queryClient = useQueryClient()
  const key = boardKeys.data(projectKey, params)

  return useMutation<void, unknown, CardMove, { previous: BoardData | undefined }>({
    // Serialise moves on this board so two fast drags cannot interleave and clobber each
    // other's optimistic snapshot.
    scope: { id: `board-${projectKey}-${params.parent ?? 'root'}` },

    mutationFn: async (move) => {
      const neighbours = {
        ...(move.previousCardId !== undefined && { previousCardId: move.previousCardId }),
        ...(move.nextCardId !== undefined && { nextCardId: move.nextCardId }),
      }

      if (move.sameColumn) {
        await boardApi.moveCard(move.card.key, neighbours)
        return
      }

      const transitions = await boardApi.fetchCardTransitions(move.card.key)
      const legal = transitions.find((t) => t.toStatusId === move.toStatusId)
      if (!legal) {
        throw new MoveError(
          `${move.card.key} can’t move to ${move.toStatusName} — the workflow doesn’t allow it.`,
        )
      }

      if (legal.id != null) {
        await boardApi.executeTransition(move.card.key, legal.id)
      } else {
        await boardApi.moveCard(move.card.key, { statusId: move.toStatusId, ...neighbours })
      }
    },

    onMutate: async (move) => {
      await queryClient.cancelQueries({ queryKey: key })
      const previous = queryClient.getQueryData<BoardData>(key)
      queryClient.setQueryData<BoardData>(key, (board) =>
        board
          ? applyMove(board, {
              cardId: move.card.id,
              toStatusId: move.toStatusId,
              toIndex: move.toIndex,
            })
          : board,
      )
      return { previous }
    },

    onError: (error, _move, context) => {
      if (context?.previous !== undefined) {
        queryClient.setQueryData(key, context.previous)
      }
      const message =
        error instanceof MoveError
          ? error.message
          : error instanceof ApiError
            ? (error.problem?.detail ?? 'The move was rejected. It has been put back.')
            : 'Something went wrong moving the card. It has been put back.'
      toast('error', message)
    },

    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: key })
    },
  })
}
