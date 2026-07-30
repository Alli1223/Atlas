import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as settingsApi from './api'
import type { CreateCredentialInput, Credential } from './api'

/**
 * Query keys for everything integrations.
 *
 * A single object rather than scattered string literals: `invalidateQueries` and
 * `useQuery` must agree exactly, and a typo in one of them fails *silently*. Mirrors
 * `authKeys` and `projectKeys`.
 */
export const credentialKeys = {
  all: ['credentials'] as const,
  list: () => [...credentialKeys.all, 'list'] as const,
}

/** Every stored credential, as metadata. */
export function credentialsQueryOptions() {
  return queryOptions({
    queryKey: credentialKeys.list(),
    queryFn: settingsApi.fetchCredentials,
  })
}

/** Every stored credential, as metadata. */
export function useCredentials() {
  return useQuery(credentialsQueryOptions())
}

/**
 * Stores a new credential and refreshes the list.
 *
 * Invalidates rather than pushing the returned row into the cached list: the server orders
 * the list and resolves the effective status pill against its own clock, and a client that
 * guessed either would be wrong the moment an expiry lapsed.
 */
export function useCreateCredential() {
  const queryClient = useQueryClient()

  return useMutation<Credential, ApiError, CreateCredentialInput>({
    mutationFn: settingsApi.createCredential,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: credentialKeys.list() })
    },
  })
}

/** The arguments to a replace: which credential to supersede, and what to store instead. */
export interface ReplaceCredentialInput {
  /** The id of the credential being replaced. */
  oldId: string
  /** The new credential to store. */
  next: CreateCredentialInput
}

/**
 * Replaces a credential: deletes the old one, then stores the new.
 *
 * Delete *then* create, in that order, so the new key may reuse the old one's label — the
 * backend's uniqueness constraint is per (provider, label), and creating first would 409
 * against the row we are about to remove. The window where neither exists is a single
 * writer transaction apart on a pool of one, and the secret being replaced is already
 * being re-supplied, so losing it is not a data loss. `onSettled` refreshes the list on
 * either outcome, so a create that fails after the delete still leaves the UI truthful.
 */
export function useReplaceCredential() {
  const queryClient = useQueryClient()

  return useMutation<Credential, ApiError, ReplaceCredentialInput>({
    mutationFn: async ({ oldId, next }) => {
      await settingsApi.deleteCredential(oldId)
      return settingsApi.createCredential(next)
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: credentialKeys.list() })
    },
  })
}

/** Deletes a credential and refreshes the list. */
export function useDeleteCredential() {
  const queryClient = useQueryClient()

  return useMutation<void, ApiError, string>({
    mutationFn: settingsApi.deleteCredential,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: credentialKeys.list() })
    },
  })
}

/**
 * Validates a credential and folds the fresh metadata straight into the cached list.
 *
 * `setQueryData` rather than a blind invalidate: the validate response *is* the updated
 * row, so the pill flips the instant the probe returns with no second round-trip — and the
 * "Validate now" button feels like it did something. A follow-up invalidate would only
 * re-fetch an answer already in hand and race the pill update.
 */
export function useValidateCredential() {
  const queryClient = useQueryClient()

  return useMutation<Credential, ApiError, string>({
    mutationFn: settingsApi.validateCredential,
    onSuccess: (updated) => {
      queryClient.setQueryData<Credential[]>(credentialKeys.list(), (current) =>
        current?.map((credential) => (credential.id === updated.id ? updated : credential)),
      )
    },
  })
}
