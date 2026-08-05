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
  AgentSession,
  BranchCreated,
  Card,
  CardGitLink,
  CardPatch,
  Comment,
  ExecuteTransitionInput,
  LinkRepoInput,
  ProjectMember,
  ProjectRepo,
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
  repo: (projectKey: string) => [...cardKeys.all, 'repo', projectKey] as const,
  gitLinks: (key: string) => [...cardKeys.all, 'git-links', key] as const,
  githubCredentials: () => [...cardKeys.all, 'github-credentials'] as const,
  activity: (key: string) => [...cardKeys.all, 'activity', key] as const,
  credentialRepos: (credentialId: string) =>
    [...cardKeys.all, 'credential-repos', credentialId] as const,
  agentSessions: (key: string) => [...cardKeys.all, 'agent-sessions', key] as const,
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

// ---------------------------------------------------------------------------
// GitHub: the project↔repo link and the card's branches / PRs / commits.
// ---------------------------------------------------------------------------

/** The repo linked to the card's project, or `null`. Cached briefly — it rarely changes. */
export function useProjectRepo(projectKey: string) {
  return useQuery({
    queryKey: cardKeys.repo(projectKey),
    queryFn: () => cardApi.fetchProjectRepo(projectKey),
    staleTime: 60_000,
  })
}

/** A card's git links (branches, PRs, commits). */
export function useCardGitLinks(cardKey: string) {
  return useQuery({
    queryKey: cardKeys.gitLinks(cardKey),
    queryFn: () => cardApi.fetchCardGitLinks(cardKey),
  })
}

/**
 * The GitHub credentials to pick from when linking. Fetched lazily via `enabled` — listing
 * credentials is admin-only, so it only runs when the (admin-gated) link dialog opens.
 */
export function useGithubCredentials(enabled = true) {
  return useQuery({
    queryKey: cardKeys.githubCredentials(),
    queryFn: cardApi.fetchGithubCredentials,
    enabled,
    staleTime: 60_000,
  })
}

/**
 * A card's live commits and CI status. Only enable this once the card has a branch — the
 * server 409s otherwise, and there is nothing to show before then anyway.
 *
 * Polls every 15s while the CI state is still `running`, so a check that finishes shows up
 * without the user having to reload; it stops polling the moment it isn't.
 */
export function useCardActivity(cardKey: string, enabled: boolean) {
  return useQuery({
    queryKey: cardKeys.activity(cardKey),
    queryFn: () => cardApi.fetchCardActivity(cardKey),
    enabled,
    refetchInterval: (query) => (query.state.data?.ciStatus === 'running' ? 15_000 : false),
  })
}

/**
 * The repos a chosen credential can see, for the link dialog's picker. Only the first page
 * (30, most-recently-pushed) — enough to cover the repo someone just pushed a project to,
 * which is the common case; anything further back is still reachable by typing owner/repo
 * directly, so this is a convenience, not the only way in.
 */
export function useCredentialRepos(credentialId: string, enabled: boolean) {
  return useQuery({
    queryKey: cardKeys.credentialRepos(credentialId),
    queryFn: () => cardApi.fetchCredentialRepos(credentialId),
    enabled,
    staleTime: 30_000,
  })
}

/** Creates a branch from the card. The branch becomes a git link, so refetch the list. */
export function useCreateBranch(cardKey: string) {
  const queryClient = useQueryClient()
  return useMutation<BranchCreated, ApiError, void>({
    mutationFn: () => cardApi.createBranch(cardKey),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cardKeys.gitLinks(cardKey) })
    },
  })
}

/** Opens a PR from the card's branch, folding the result straight into the git-links cache. */
export function useCreatePr(cardKey: string) {
  const queryClient = useQueryClient()
  return useMutation<CardGitLink, ApiError, void>({
    mutationFn: () => cardApi.createPr(cardKey),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cardKeys.gitLinks(cardKey) })
    },
  })
}

// ---------------------------------------------------------------------------
// Claude Code agent sessions: "Run with Claude" on a card.
// ---------------------------------------------------------------------------

/**
 * A card's agent sessions, most recent first — polled every 3s while the newest one is still
 * `running`, the same `refetchInterval`-as-a-function-of-data idiom `useCardActivity` uses for
 * a CI check. Polling the list rather than a separate single-session endpoint keeps this to
 * one query: the list is already ordered newest-first, so its head *is* "the current run".
 */
export function useCardAgentSessions(cardKey: string) {
  return useQuery({
    queryKey: cardKeys.agentSessions(cardKey),
    queryFn: () => cardApi.fetchCardAgentSessions(cardKey),
    refetchInterval: (query) => (query.state.data?.[0]?.status === 'running' ? 3_000 : false),
  })
}

/** Starts a run, prepending it into the session list rather than refetching. */
export function useStartAgentSession(cardKey: string) {
  const queryClient = useQueryClient()
  return useMutation<AgentSession, ApiError, void>({
    mutationFn: () => cardApi.startAgentSession(cardKey),
    onSuccess: (session) => {
      queryClient.setQueryData<AgentSession[]>(cardKeys.agentSessions(cardKey), (list) => [
        session,
        ...(list ?? []),
      ])
    },
  })
}

/** Links a repo to the card's project, folding the result straight into the repo cache. */
export function useLinkRepo(projectKey: string) {
  const queryClient = useQueryClient()
  return useMutation<ProjectRepo, ApiError, LinkRepoInput>({
    mutationFn: (input) => cardApi.linkRepo(projectKey, input),
    onSuccess: (repo) => {
      queryClient.setQueryData(cardKeys.repo(projectKey), repo)
    },
  })
}

/** Unlinks the project's repo. */
export function useUnlinkRepo(projectKey: string) {
  const queryClient = useQueryClient()
  return useMutation<void, ApiError, void>({
    mutationFn: () => cardApi.unlinkRepo(projectKey),
    onSuccess: () => {
      queryClient.setQueryData(cardKeys.repo(projectKey), null)
    },
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
