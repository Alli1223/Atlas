import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render } from '@testing-library/react'
import { type ReactElement } from 'react'

import type { Cycle } from './api'

/** Test helper for cycles components that need a QueryClient but not the router. */
export function renderWithClient(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const result = render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>)
  return { ...result, queryClient }
}

/** A cycle, overridable per test. */
export function makeCycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 'cycle-1',
    projectId: 'project-1',
    name: 'Sprint 1',
    goal: null,
    startDate: null,
    endDate: null,
    state: 'future',
    position: 0,
    createdAt: '2026-07-16T10:00:00Z',
    updatedAt: '2026-07-16T10:00:00Z',
    ...overrides,
  }
}
