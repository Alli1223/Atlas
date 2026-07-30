import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render } from '@testing-library/react'
import { type ReactElement } from 'react'
import { vi } from 'vitest'

import type { Credential } from './api'
import { credentialKeys } from './queries'

/**
 * Test helpers for the integrations feature. Not a `.test.tsx` file, so vitest does not run
 * it as a suite.
 */

/** Builds a credential DTO with sane defaults; override any field per test. */
export function credential(overrides: Partial<Credential> = {}): Credential {
  return {
    id: crypto.randomUUID(),
    provider: 'github',
    label: 'work laptop',
    lastFour: 'a1b2',
    status: 'valid',
    expiresAt: null,
    scopes: [],
    lastValidatedAt: '2026-07-20T10:00:00Z',
    createdAt: '2026-07-01T10:00:00Z',
    updatedAt: '2026-07-20T10:00:00Z',
    ...overrides,
  }
}

/**
 * Renders `ui` inside a fresh QueryClient whose credentials list is pre-seeded with
 * `credentials`. Seeding the cache directly avoids a fetch round-trip for components that
 * only *read* the list (the banner, the page body).
 */
export function renderWithCredentials(ui: ReactElement, credentials: Credential[]) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  queryClient.setQueryData(credentialKeys.list(), credentials)

  return {
    ...render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>),
    queryClient,
  }
}

/** Renders `ui` inside a fresh, empty QueryClient — for components that mutate. */
export function renderWithClient(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })

  return {
    ...render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>),
    queryClient,
  }
}

/** Builds an `application/json` Response, the way the backend would. */
export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

/**
 * Stubs `fetch` with a `METHOD /path` → Response router, recording every request body it
 * saw so a test can prove what did (and did not) go over the wire.
 */
export function stubFetch(routes: Record<string, () => Response>) {
  const bodies: string[] = []

  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : undefined
      const url =
        input instanceof Request ? input.url : input instanceof URL ? input.href : input
      const method = (request ? request.method : init?.method) ?? 'GET'
      const path = url.replace(/^https?:\/\/[^/]+/, '')
      const key = `${method.toUpperCase()} ${path}`

      const body = request ? await request.clone().text() : ((init?.body as string) ?? '')
      if (body) bodies.push(body)

      const handler = routes[key]
      if (!handler) return Promise.reject(new Error(`unstubbed request: ${key}`))
      return handler()
    }),
  )

  return { bodies }
}
