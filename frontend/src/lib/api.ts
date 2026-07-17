import createClient from 'openapi-fetch'

import type { paths } from './api-schema'

/**
 * The versioned API prefix, mirroring `crate::api::API_V1_PREFIX`.
 *
 * Exported for reference and for the rare hand-rolled `fetch`. It is deliberately NOT the
 * client's `baseUrl`: utoipa emits **absolute** paths (`/api/v1/auth/login`, and `/healthz`
 * outside the prefix entirely) with no `servers` entry, so the generated `paths` keys
 * already carry the prefix. Setting it as a baseUrl as well would request
 * `/api/v1/api/v1/auth/login`, and would put `/healthz` out of reach.
 */
export const API_BASE_URL = '/api/v1'

/**
 * The origin the API is served from: this one.
 *
 * Atlas is self-hosted and single-origin — in production the Axum binary serves these very
 * assets, and in dev Vite proxies `/api` to it — so the API's origin is always the page's.
 *
 * It is spelled out rather than left implicit because openapi-fetch builds a `Request`, and
 * `new Request('/api/v1/auth/me')` is only legal where something can resolve it against a
 * document base. Browsers can; Node's `Request` (the one jsdom tests get, since jsdom
 * implements no fetch stack) cannot, and throws "Failed to parse URL". An absolute
 * same-origin URL is identical on the wire and works in both.
 */
function apiOrigin(): string {
  return typeof window === 'undefined' ? '' : window.location.origin
}

/**
 * Typed API client. openapi-fetch is types-only at runtime (~6kB) — no generated
 * method-per-endpoint, so regenerating the schema produces no client diff noise, and
 * mutations stay hand-written where the optimistic board logic needs to own them.
 *
 * Auth is a server-side session cookie (HttpOnly, SameSite=Lax), never a localStorage
 * JWT — hence `credentials: 'include'` and no token plumbing here. There is no VITE_ env
 * var that may ever hold a secret: anything VITE_-prefixed is inlined into the bundle in
 * plaintext.
 *
 * `credentials: 'include'` rather than `'same-origin'`: both work through the dev proxy —
 * the browser only ever sees `localhost:5173`, so the cookie is same-origin either way —
 * but `include` is also correct when the SPA is served from a different origin than the
 * API, which the backend's CORS layer explicitly allows for. `same-origin` would silently
 * drop the cookie there, and that failure presents as "login returns 200 and I am still
 * logged out", with nothing in any log.
 */
export const api = createClient<paths>({
  // The origin only. The generated paths already carry `/api/v1` — see API_BASE_URL.
  baseUrl: apiOrigin(),
  credentials: 'include',
  headers: {
    'Content-Type': 'application/json',
  },
  // openapi-fetch reads `globalThis.fetch` **once**, when createClient runs — which is when
  // this module is first imported. Resolving it per call instead means the client follows
  // the current environment rather than a snapshot of it taken at import time. That is what
  // makes it stubbable at all: a test that replaces `globalThis.fetch` after importing this
  // module would otherwise be ignored, and would watch its assertions fail against real
  // network calls it never made.
  fetch: (request) => globalThis.fetch(request),
})

/**
 * An RFC 7807 problem document — the shape of *every* Atlas error response.
 *
 * Mirrors `crate::error::Problem`. `type` is the machine-readable identifier and the only
 * field a client may branch on; `title` and `detail` are prose written for a human and are
 * free to be re-worded in any release.
 */
export interface Problem {
  /** Stable error identifier, e.g. `urn:atlas:error:not-found`. Branch on this. */
  type: string
  /** Short human summary, stable per `type`. */
  title: string
  /** The HTTP status, repeated per RFC 7807. */
  status: number
  /** Human-readable explanation of this occurrence. */
  detail: string
  /** The request path, filled in by the backend's `problem_instance` layer. */
  instance?: string | null
}

/** Whether an unknown value is a problem document. */
export function isProblem(value: unknown): value is Problem {
  if (typeof value !== 'object' || value === null) return false
  const candidate = value as Record<string, unknown>
  return (
    typeof candidate.type === 'string' &&
    typeof candidate.title === 'string' &&
    typeof candidate.status === 'number' &&
    typeof candidate.detail === 'string'
  )
}

/**
 * A failed API call, carrying the problem document when the response had one.
 *
 * openapi-fetch hands back `{ data, error }` rather than throwing, but TanStack Query's
 * entire error model — `isError`, retries, `onError` — is built on rejected promises. So
 * every call goes through [`unwrap`], which converts one convention into the other exactly
 * once rather than at each call site.
 */
export class ApiError extends Error {
  /** The problem document, when the response carried a parseable one. */
  readonly problem: Problem | undefined
  /** The HTTP status. `0` when the request never reached the server. */
  readonly status: number

  constructor(message: string, status: number, problem?: Problem) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.problem = problem
  }

  /** The problem `type`, or `undefined` for a non-problem failure (offline, a proxy 502). */
  get type(): string | undefined {
    return this.problem?.type
  }
}

/**
 * Turns openapi-fetch's `{ data, error }` into a value, or throws an [`ApiError`].
 *
 * `data === undefined` is deliberately not folded into the error case: a 204 is a *success*
 * with no body — logout returns one — so absent data with no error is `undefined`, not a
 * failure.
 */
export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.error !== undefined) {
    // openapi-fetch JSON.parses the error body and falls back to the raw text, so `error`
    // is a Problem for every Atlas error and a string for anything else on the wire.
    const problem = isProblem(result.error) ? result.error : undefined
    const message =
      problem?.detail ??
      (typeof result.error === 'string' && result.error.trim().length > 0
        ? result.error
        : `Request failed with status ${result.response.status}`)
    throw new ApiError(message, result.response.status, problem)
  }

  if (!result.response.ok) {
    // An error status that yielded no `error` field — e.g. a bodiless 502 from a proxy.
    // Reporting it as a success would hand the caller `undefined` and blow up somewhere
    // later, a long way from the cause.
    throw new ApiError(
      `Request failed with status ${result.response.status}`,
      result.response.status,
    )
  }

  return result.data as T
}

/** WebSocket URL for live board sync and Claude Code session streams. */
export function wsUrl(path: string): string {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${protocol}//${window.location.host}/ws${path.startsWith('/') ? path : `/${path}`}`
}
