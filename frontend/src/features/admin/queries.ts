import { useMutation, useQuery } from '@tanstack/react-query'

import * as adminApi from './api'
import type { ApplyUpdateResponse } from './api'

export const adminKeys = {
  all: ['admin'] as const,
  system: () => [...adminKeys.all, 'system'] as const,
  updates: () => [...adminKeys.all, 'updates'] as const,
}

/** Polls system stats every 10 s so the page stays live without a manual refresh. */
export function useSystemStats() {
  return useQuery({
    queryKey: adminKeys.system(),
    queryFn: adminApi.fetchSystemStats,
    refetchInterval: 10_000,
  })
}

/** Checks for a newer release once per page visit — no auto-refresh needed. */
export function useUpdateStatus() {
  return useQuery({
    queryKey: adminKeys.updates(),
    queryFn: adminApi.fetchUpdateStatus,
  })
}

export function useApplyUpdate() {
  return useMutation<ApplyUpdateResponse, Error>({
    mutationFn: adminApi.applyUpdate,
  })
}
