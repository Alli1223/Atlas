import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router'
import { render } from '@testing-library/react'
import { vi } from 'vitest'

import { ThemeProvider } from '@/providers/ThemeProvider'
import { createAppRouter } from '@/router'

import type { User } from './api'

/**
 * Test helpers for anything that mounts the app behind [`AuthGate`].
 *
 * Not a `.test.tsx` file, so vitest does not try to run it as a suite; it lives beside the
 * feature rather than in `src/test/` because everything in it is auth-shaped.
 */

/** A signed-in admin, past the forced-reset gate. */
export const ADMIN: User = {
  id: '019f6e8e-6baa-7770-8184-b3c9405cc2d3',
  username: 'Admin',
  email: null,
  displayName: 'Administrator',
  avatarUrl: null,
  role: 'admin',
  isActive: true,
  mustChangePassword: false,
  createdAt: '2026-07-17T05:30:55.274014Z',
  updatedAt: '2026-07-17T05:30:55.274014Z',
  lastLoginAt: '2026-07-17T05:31:29.498844Z',
}

/** The freshly seeded admin: authenticated, but locked to the change-password screen. */
export const GATED_ADMIN: User = { ...ADMIN, mustChangePassword: true }

/** Builds an `application/json` Response, the way the backend would. */
export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

/**
 * Builds an `application/problem+json` Response — the shape of every Atlas error.
 *
 * Real documents, not `{ error: '...' }`: the client branches on `type`, so a stub that
 * omitted it would let a broken marker check pass.
 */
export function problemResponse(type: string, status: number, detail = 'Test problem'): Response {
  return new Response(
    JSON.stringify({ type, title: 'Test', status, detail, instance: '/api/v1/test' }),
    { status, headers: { 'content-type': 'application/problem+json' } },
  )
}

/** `METHOD /path` → the response to give. */
export type ApiStub = Record<string, () => Response>

/**
 * Stubs `fetch` with a tiny router.
 *
 * This only works because `api.ts` resolves `globalThis.fetch` per call rather than letting
 * openapi-fetch snapshot it at import time — see the note there. If that ever regresses,
 * these tests do not fail: they quietly make real network calls and time out.
 *
 * An unrouted request rejects rather than 404ing: a test that quietly calls an endpoint
 * nobody stubbed is a test whose subject is not what its name says.
 */
export function stubFetch(routes: ApiStub) {
  const calls: string[] = []

  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = input instanceof Request ? input.url : String(input)
      const method = (input instanceof Request ? input.method : init?.method) ?? 'GET'
      const path = url.replace(/^https?:\/\/[^/]+/, '')
      const key = `${method.toUpperCase()} ${path}`
      calls.push(key)

      const handler = routes[key]
      if (!handler) {
        return Promise.reject(new Error(`unstubbed request: ${key}`))
      }
      return Promise.resolve(handler())
    }),
  )

  return { calls }
}

/** Mounts the real app — root route, route tree, providers — at `initialPath`. */
export function renderApp(initialPath = '/') {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [initialPath] }))

  const result = render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  )

  return { ...result, router, queryClient }
}
