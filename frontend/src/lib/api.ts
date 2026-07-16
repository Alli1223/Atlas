import createClient from 'openapi-fetch'

import type { paths } from './api-schema'

/**
 * The API base. Same prefix on both sides — Vite proxies /api to the Axum backend in dev,
 * and in production the Axum binary serves these assets itself, so no rewrite exists in
 * either direction. That removes a whole class of "works in dev, 404s in prod" bugs.
 */
export const API_BASE_URL = '/api/v1'

/**
 * Typed API client. openapi-fetch is types-only at runtime (~6kB) — no generated
 * method-per-endpoint, so regenerating the schema produces no client diff noise, and
 * mutations stay hand-written where the optimistic board logic needs to own them.
 *
 * Auth is a server-side session cookie (HttpOnly, SameSite=Lax), never a localStorage
 * JWT — hence `credentials: 'include'` and no token plumbing here. There is no VITE_ env
 * var that may ever hold a secret: anything VITE_-prefixed is inlined into the bundle in
 * plaintext.
 */
export const api = createClient<paths>({
  baseUrl: API_BASE_URL,
  credentials: 'include',
  headers: {
    'Content-Type': 'application/json',
  },
})

/** WebSocket URL for live board sync and Claude Code session streams. */
export function wsUrl(path: string): string {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${protocol}//${window.location.host}/ws${path.startsWith('/') ? path : `/${path}`}`
}
