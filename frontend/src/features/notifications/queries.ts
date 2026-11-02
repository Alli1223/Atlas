import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import * as notificationsApi from './api'

export const notificationKeys = {
  all: ['notifications'] as const,
  inbox: () => [...notificationKeys.all, 'inbox'] as const,
  preferences: () => [...notificationKeys.all, 'preferences'] as const,
}

/** The bell's contents. Refreshed by the live channel; polled slowly as a fallback. */
export function useInbox() {
  return useQuery({
    queryKey: notificationKeys.inbox(),
    queryFn: notificationsApi.fetchInbox,
    refetchInterval: 120_000,
  })
}

export function useMarkRead() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: notificationsApi.markRead,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: notificationKeys.inbox() }),
  })
}

export function useMarkAllRead() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: notificationsApi.markAllRead,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: notificationKeys.inbox() }),
  })
}

export function usePreferences() {
  return useQuery({
    queryKey: notificationKeys.preferences(),
    queryFn: notificationsApi.fetchPreferences,
  })
}

export function useSavePreferences() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: notificationsApi.savePreferences,
    onSuccess: (prefs) => queryClient.setQueryData(notificationKeys.preferences(), prefs),
  })
}
