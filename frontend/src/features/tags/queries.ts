import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as tagsApi from './api'
import type { CreateTagInput, MergeTagInput, Tag, TagUsage, UpdateTagInput } from './api'

/**
 * Query keys for everything tags.
 *
 * A single object rather than scattered string literals: `invalidateQueries` and
 * `useQuery` must agree exactly, and a typo in one of them fails *silently* — the screen
 * simply keeps showing stale data. Mirrors `authKeys` in `features/auth/queries.ts`.
 */
export const tagKeys = {
  all: ['tags'] as const,
  forProject: (projectKey: string) => [...tagKeys.all, 'project', projectKey] as const,
  forCard: (cardKey: string) => [...tagKeys.all, 'card', cardKey] as const,
}

/** Every tag a project offers, with usage counts. */
export function projectTagsQueryOptions(projectKey: string) {
  return queryOptions({
    queryKey: tagKeys.forProject(projectKey),
    queryFn: () => tagsApi.fetchProjectTags(projectKey),
  })
}

/** The tags on one card. */
export function cardTagsQueryOptions(cardKey: string) {
  return queryOptions({
    queryKey: tagKeys.forCard(cardKey),
    queryFn: () => tagsApi.fetchCardTags(cardKey),
  })
}

/** Every tag a project offers, with usage counts. */
export function useProjectTags(projectKey: string) {
  return useQuery(projectTagsQueryOptions(projectKey))
}

/** The tags on one card. */
export function useCardTags(cardKey: string) {
  return useQuery(cardTagsQueryOptions(cardKey))
}

/**
 * Creates a tag — the picker's create-on-the-fly path.
 *
 * Invalidates the project's list rather than pushing the new tag into it: the server
 * assigns the id and orders the list by name, and a client that guesses either would be
 * wrong the moment two people type at once.
 */
export function useCreateTag(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Tag, ApiError, Omit<CreateTagInput, 'projectKey'>>({
    mutationFn: (input) => tagsApi.createTag({ ...input, projectKey }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: tagKeys.forProject(projectKey) })
    },
  })
}

/**
 * Renames and/or recolours a tag.
 *
 * Invalidates **every** tag query, not just this project's: the tag is on cards whose
 * chips now read differently, and a global tag is on cards in projects this mutation
 * never named. Scoping this to one key is how you get a board still showing the old name
 * until someone reloads.
 */
export function useUpdateTag() {
  const queryClient = useQueryClient()

  return useMutation<Tag, ApiError, UpdateTagInput>({
    mutationFn: tagsApi.updateTag,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: tagKeys.all })
    },
  })
}

/** Deletes a tag, taking it off every card that carried it. */
export function useDeleteTag() {
  const queryClient = useQueryClient()

  return useMutation<void, ApiError, string>({
    mutationFn: tagsApi.deleteTag,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: tagKeys.all })
    },
  })
}

/** Merges one tag into another. The source stops existing. */
export function useMergeTag() {
  const queryClient = useQueryClient()

  return useMutation<Tag, ApiError, MergeTagInput>({
    mutationFn: tagsApi.mergeTag,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: tagKeys.all })
    },
  })
}

/**
 * Puts a tag on a card, optimistically.
 *
 * # Why optimistic here
 *
 * `CLAUDE.md`: card interactions "must feel instant". Tagging is a click on a chip, and a
 * chip that appears 80ms later reads as a missed click — so the user clicks again. The
 * rollback matters as much as the optimism: on failure the chip must *leave*, or the UI
 * is lying about what the server holds.
 *
 * `cancelQueries` first, because an in-flight refetch that resolves after this
 * `setQueryData` would overwrite the optimistic chip with a list that predates it.
 */
export function useAttachTag(cardKey: string) {
  const queryClient = useQueryClient()
  const key = tagKeys.forCard(cardKey)

  return useMutation<Tag[], ApiError, Tag, { previous: Tag[] | undefined }>({
    mutationFn: (tag) => tagsApi.attachTag({ cardKey, tagId: tag.id }),

    onMutate: async (tag) => {
      await queryClient.cancelQueries({ queryKey: key })
      const previous = queryClient.getQueryData<Tag[]>(key)

      queryClient.setQueryData<Tag[]>(key, (current) => {
        const tags = current ?? []
        // The backend is idempotent about this; the cache must be too, or a double-click
        // renders the same chip twice until the response lands.
        if (tags.some((t) => t.id === tag.id)) return tags
        // Sorted by name, matching the server's ORDER BY — otherwise the chip jumps when
        // the real answer arrives.
        return [...tags, tag].sort((a, b) => a.name.localeCompare(b.name))
      })

      return { previous }
    },

    onError: (_error, _tag, context) => {
      if (context?.previous !== undefined) queryClient.setQueryData(key, context.previous)
    },

    onSuccess: (tags) => {
      // The server's answer replaces the guess.
      queryClient.setQueryData(key, tags)
    },

    onSettled: () => {
      // The usage counts moved, so the project list is stale either way.
      void queryClient.invalidateQueries({ queryKey: tagKeys.all, exact: false })
    },
  })
}

/** Takes a tag off a card, optimistically. Same reasoning as [`useAttachTag`]. */
export function useDetachTag(cardKey: string) {
  const queryClient = useQueryClient()
  const key = tagKeys.forCard(cardKey)

  return useMutation<void, ApiError, string, { previous: Tag[] | undefined }>({
    mutationFn: (tagId) => tagsApi.detachTag({ cardKey, tagId }),

    onMutate: async (tagId) => {
      await queryClient.cancelQueries({ queryKey: key })
      const previous = queryClient.getQueryData<Tag[]>(key)

      queryClient.setQueryData<Tag[]>(key, (current) =>
        (current ?? []).filter((t) => t.id !== tagId),
      )

      return { previous }
    },

    onError: (_error, _tagId, context) => {
      if (context?.previous !== undefined) queryClient.setQueryData(key, context.previous)
    },

    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: tagKeys.all, exact: false })
    },
  })
}

/** Convenience: a project's tags as the picker wants them, already loaded. */
export type ProjectTagOption = TagUsage
