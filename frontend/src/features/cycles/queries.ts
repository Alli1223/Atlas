import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as cyclesApi from './api'
import type {
  CompleteCycleInput,
  CreateCycleInput,
  Cycle,
  ReopenCycleInput,
  StartCycleInput,
  UpdateCycleInput,
} from './api'

/**
 * Query keys for everything cycles.
 *
 * A single object rather than scattered string literals: `invalidateQueries` and `useQuery`
 * must agree exactly, and a typo in one of them fails *silently*. Mirrors `tagKeys`.
 */
export const cycleKeys = {
  all: ['cycles'] as const,
  forProject: (projectKey: string) => [...cycleKeys.all, 'project', projectKey] as const,
  forCard: (cardKey: string) => [...cycleKeys.all, 'card', cardKey] as const,
}

/** A project's cycles: active first, then future, then closed. */
export function projectCyclesQueryOptions(projectKey: string) {
  return queryOptions({
    queryKey: cycleKeys.forProject(projectKey),
    queryFn: () => cyclesApi.fetchProjectCycles(projectKey),
  })
}

/** A project's cycles. */
export function useProjectCycles(projectKey: string) {
  return useQuery(projectCyclesQueryOptions(projectKey))
}

/** The cycle a card currently belongs to, or `null`. */
export function useCardCycle(cardKey: string) {
  return useQuery({
    queryKey: cycleKeys.forCard(cardKey),
    queryFn: () => cyclesApi.fetchCardCycle(cardKey),
  })
}

/** Creates a cycle. Invalidates the project's list — the server assigns the id and position. */
export function useCreateCycle(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Cycle, ApiError, Omit<CreateCycleInput, 'projectKey'>>({
    mutationFn: (input) => cyclesApi.createCycle({ ...input, projectKey }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forProject(projectKey) })
    },
  })
}

/** Renames a cycle and/or edits its goal. */
export function useUpdateCycle(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Cycle, ApiError, UpdateCycleInput>({
    mutationFn: cyclesApi.updateCycle,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forProject(projectKey) })
    },
  })
}

/** Starts a cycle. Invalidates the project's list — the state machine may affect siblings. */
export function useStartCycle(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Cycle, ApiError, StartCycleInput>({
    mutationFn: cyclesApi.startCycle,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forProject(projectKey) })
    },
  })
}

/**
 * Completes a cycle, carrying its incomplete cards as directed.
 *
 * Invalidates every card-cycle query too, not just the project's list: a card carried into
 * another cycle (or back to the backlog) now has a different answer, and there is no cheap
 * way from here to know which cards those were.
 */
export function useCompleteCycle(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Cycle, ApiError, CompleteCycleInput>({
    mutationFn: cyclesApi.completeCycle,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forProject(projectKey) })
      void queryClient.invalidateQueries({ queryKey: cycleKeys.all, exact: false })
    },
  })
}

/** Reopens a closed cycle. */
export function useReopenCycle(projectKey: string) {
  const queryClient = useQueryClient()

  return useMutation<Cycle, ApiError, ReopenCycleInput>({
    mutationFn: cyclesApi.reopenCycle,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forProject(projectKey) })
    },
  })
}

/** Adds a card to a cycle, refetching its current-cycle query. */
export function useAddCardToCycle(cardKey: string) {
  const queryClient = useQueryClient()

  return useMutation<void, ApiError, string>({
    mutationFn: (cycleId) => cyclesApi.addCardToCycle(cardKey, cycleId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: cycleKeys.forCard(cardKey) })
    },
  })
}

/** Removes a card from its current cycle. */
export function useRemoveCardFromCycle(cardKey: string) {
  const queryClient = useQueryClient()

  return useMutation<void, ApiError, void>({
    mutationFn: () => cyclesApi.removeCardFromCycle(cardKey),
    onSuccess: () => {
      queryClient.setQueryData(cycleKeys.forCard(cardKey), null)
    },
  })
}
