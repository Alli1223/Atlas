import {
  queryOptions,
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as cardApi from './api'
import type {
  Card,
  CardPatch,
  Comment,
  ExecuteTransitionInput,
  ProjectMember,
} from './api'
import { projectKeyOf } from './api'

/**
 * Query keys for the card detail view.
 *
 * One object rather than scattered literals: a mutation's `invalidateQueries` and a hook's
 * `useQuery` must agree exactly, and a typo fails *silently* — the screen just keeps showing
 * stale data. Mirrors `authKeys` and `tagKeys`.
 */
export const cardKeys = {
  all: ['card-detail'] as const,
  card: (key: string) => [...cardKeys.all, 'card', key] as const,
  comments: (key: string) => [...cardKeys.all, 'comments', key] as const,
  history: (key: string) => [...cardKeys.all, 'history', key] as const,
  children: (key: string) => [...cardKeys.all, 'children', key] as const,
  transitions: (key: string) => [...cardKeys.all, 'transitions', key] as const,
  statuses: (projectKey: string) => [...cardKeys.all, 'statuses', projectKey] as const,
  priorities: (projectKey: string) => [...cardKeys.all, 'priorities', projectKey] as const,
  resolutions: (projectKey: string) => [...cardKeys.all, 'resolutions', projectKey] as const,
  cardTypes: (projectKey: string) => [...cardKeys.all, 'card-types', projectKey] as const,
  members: (projectKey: string) => [...cardKeys.all, 'members', projectKey] as const,
}

/** Shared options so a route loader can warm the same cache the component reads. */
export function cardQueryOptions(key: string) {
  return queryOptions({ queryKey: cardKeys.card(key), queryFn: () => cardApi.fetchCard(key) })
}

export function useCard(key: string) {
  return useQuery(cardQueryOptions(key))
}

export function useComments(key: string) {
  return useQuery({ queryKey: cardKeys.comments(key), queryFn: () => cardApi.fetchComments(key) })
}

export function useHistory(key: string, enabled = true) {
  return useQuery({
    queryKey: cardKeys.history(key),
    queryFn: () => cardApi.fetchHistory(key),
    enabled,
  })
}

export function useChildren(key: string) {
  return useQuery({ queryKey: cardKeys.children(key), queryFn: () => cardApi.fetchChildren(key) })
}

export function useTransitions(key: string) {
  return useQuery({
    queryKey: cardKeys.transitions(key),
    queryFn: () => cardApi.fetchTransitions(key),
  })
}

export function useStatuses(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.statuses(projectKey),
    queryFn: () => cardApi.fetchStatuses(projectKey),
    staleTime: 60_000,
  })
}

export function usePriorities(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.priorities(projectKey),
    queryFn: () => cardApi.fetchPriorities(projectKey),
    staleTime: 60_000,
  })
}

export function useResolutions(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.resolutions(projectKey),
    queryFn: () => cardApi.fetchResolutions(projectKey),
    staleTime: 60_000,
  })
}

export function useCardTypes(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.cardTypes(projectKey),
    queryFn: () => cardApi.fetchCardTypes(projectKey),
    staleTime: 60_000,
  })
}

export function useMembers(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.members(projectKey),
    queryFn: () => cardApi.fetchMembers(projectKey),
    staleTime: 60_000,
  })
}

/**
 * A project's cards indexed by id, for resolving a card's parent to a linkable key.
 *
 * `enabled` gates the fetch: a card with no parent never triggers it. Returns a Map so the
 * sidebar lookup is O(1) rather than a scan per render.
 */
export function useCardIndex(projectKey: string, enabled: boolean) {
  return useQuery({
    queryKey: [...cardKeys.all, 'card-index', projectKey],
    queryFn: async () => {
      const cards = await cardApi.fetchProjectCards(projectKey)
      const index = new Map<string, { key: string; summary: string }>()
      for (const card of cards) index.set(card.id, { key: card.key, summary: card.summary })
      return index
    },
    enabled,
    staleTime: 60_000,
  })
}

/**
 * Edits a card field, optimistically, with rollback on failure.
 *
 * # The four rules, and why each is load-bearing
 *
 * `CLAUDE.md` requires card interactions to feel instant, and inline edit is the case that
 * proves it: click a field, type, blur, and the new value must already be there. So the new
 * value is written into the cache *before* the request goes out —
 *
 * 1. **cancel in-flight queries first**, or a `GET /cards/{key}` already on the wire resolves
 *    after the optimistic write and visibly reverts the field under the user's cursor;
 * 2. **snapshot before mutating**, so there is something exact to roll back to;
 * 3. **roll back in `onError`** from that snapshot — the optimism is a lie the moment the
 *    server rejects it, and a field left showing an unsaved value is worse than a slow one;
 * 4. **reconcile in `onSettled`, not `onSuccess`** — `onSuccess` skips the rollback path, so
 *    after a failure the client would stay authoritative over a value the server never took.
 *
 * The server's response replaces the guess on success (it also carries fields the edit did
 * not touch, e.g. an auto-set resolution when the status became Done), and the changelog is
 * always invalidated because any successful edit wrote history rows.
 */
export function usePatchCard(key: string) {
  const queryClient = useQueryClient()
  const cacheKey = cardKeys.card(key)

  return useMutation<Card, ApiError, CardPatch, { previous: Card | undefined }>({
    mutationFn: (patch) => cardApi.patchCard(key, patch),

    onMutate: async (patch) => {
      await queryClient.cancelQueries({ queryKey: cacheKey })
      const previous = queryClient.getQueryData<Card>(cacheKey)

      if (previous) {
        // Only the keys the patch actually carried, so an absent field is left untouched
        // rather than overwritten with undefined.
        queryClient.setQueryData<Card>(cacheKey, { ...previous, ...stripUndefined(patch) })
      }

      return { previous }
    },

    onError: (_error, _patch, context) => {
      if (context?.previous !== undefined) queryClient.setQueryData(cacheKey, context.previous)
    },

    onSuccess: (card) => {
      queryClient.setQueryData(cacheKey, card)
    },

    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: cardKeys.card(key) })
      // A status change auto-sets resolution and writes history, and any edit writes history.
      void queryClient.invalidateQueries({ queryKey: cardKeys.history(key) })
      void queryClient.invalidateQueries({ queryKey: cardKeys.transitions(key) })
    },
  })
}

/** Drops keys whose value is `undefined`, so an optimistic merge cannot blank a field. */
function stripUndefined<T extends object>(value: T): Partial<T> {
  return Object.fromEntries(
    Object.entries(value).filter(([, v]) => v !== undefined),
  ) as Partial<T>
}

/** Takes a workflow transition, then re-syncs the card, its history and its next moves. */
export function useExecuteTransition(key: string) {
  const queryClient = useQueryClient()

  return useMutation<Card, ApiError, { transitionId: string; input?: ExecuteTransitionInput }>({
    mutationFn: ({ transitionId, input }) => cardApi.executeTransition(key, transitionId, input),
    onSuccess: (card) => {
      queryClient.setQueryData(cardKeys.card(key), card)
    },
    onSettled: () => {
      invalidateCard(queryClient, key)
    },
  })
}

/** Posts a comment, then refetches the list (the server assigns id and timestamps). */
export function useAddComment(key: string) {
  const queryClient = useQueryClient()
  return useMutation<Comment, ApiError, string>({
    mutationFn: (body) => cardApi.postComment(key, body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cardKeys.comments(key) })
    },
  })
}

/** Edits a comment. Refetches so `editedAt` and the new body come from one authority. */
export function useEditComment(key: string) {
  const queryClient = useQueryClient()
  return useMutation<Comment, ApiError, { id: string; body: string }>({
    mutationFn: ({ id, body }) => cardApi.patchComment(id, body),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cardKeys.comments(key) })
    },
  })
}

/** Deletes a comment, optimistically dropping it from the list. */
export function useDeleteComment(key: string) {
  const queryClient = useQueryClient()
  const cacheKey = cardKeys.comments(key)

  return useMutation<void, ApiError, string, { previous: Comment[] | undefined }>({
    mutationFn: (id) => cardApi.deleteComment(id),
    onMutate: async (id) => {
      await queryClient.cancelQueries({ queryKey: cacheKey })
      const previous = queryClient.getQueryData<Comment[]>(cacheKey)
      queryClient.setQueryData<Comment[]>(cacheKey, (list) => (list ?? []).filter((c) => c.id !== id))
      return { previous }
    },
    onError: (_error, _id, context) => {
      if (context?.previous !== undefined) queryClient.setQueryData(cacheKey, context.previous)
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: cacheKey })
    },
  })
}

/** Invalidates everything a card write can have changed. */
function invalidateCard(queryClient: QueryClient, key: string) {
  void queryClient.invalidateQueries({ queryKey: cardKeys.card(key) })
  void queryClient.invalidateQueries({ queryKey: cardKeys.history(key) })
  void queryClient.invalidateQueries({ queryKey: cardKeys.transitions(key) })
}

/** The display name for a member id, falling back to something honest when unknown. */
export function memberName(members: ProjectMember[] | undefined, userId: string | null): string {
  if (userId == null) return 'Unassigned'
  const match = members?.find((m) => m.userId === userId)
  return match?.displayName ?? 'Unknown user'
}

export { projectKeyOf }
