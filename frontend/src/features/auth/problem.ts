import { ApiError } from '@/lib/api'

/**
 * The auth-specific problem `type`s, mirroring `crate::auth::problem`.
 *
 * These strings are the contract with the backend. Everything the client decides about an
 * auth failure is decided from one of them — never from `title` or `detail`, which are
 * prose and one copy-edit away from breaking a `String.includes` check written against
 * them.
 */
export const PROBLEM_TYPE = {
  /** 403 that means "go to the change-password screen", NOT "you are not allowed". */
  passwordChangeRequired: 'urn:atlas:error:password-change-required',
  /** 429: too many failed sign-ins for this username or address. */
  lockedOut: 'urn:atlas:error:locked-out',
  /** 401: no session, or bad credentials. */
  unauthorized: 'urn:atlas:error:unauthorized',
  /** 403: authenticated, but not permitted. */
  forbidden: 'urn:atlas:error:forbidden',
  /** 422: the request was well-formed but broke a rule (e.g. the password policy). */
  validation: 'urn:atlas:error:validation',
} as const

/**
 * Whether an error is the forced-password-change gate.
 *
 * The marker is the whole point: the backend returns 403 both for "reset your password"
 * and for "you are not an admin", and the client must send the user to a different place
 * for each. Matching on the `type` URN is what keeps those apart — a check against the
 * `detail` text would pass review and break the first time the message is reworded.
 */
export function isPasswordChangeRequired(error: unknown): boolean {
  return error instanceof ApiError && error.type === PROBLEM_TYPE.passwordChangeRequired
}

/** Whether an error means "you have no valid session". */
export function isUnauthorized(error: unknown): boolean {
  return error instanceof ApiError && error.status === 401
}

/** Whether an error is a login lockout. */
export function isLockedOut(error: unknown): boolean {
  return error instanceof ApiError && error.type === PROBLEM_TYPE.lockedOut
}

/**
 * The message to show a user for a failed auth call.
 *
 * The backend writes `detail` for humans — "Invalid username or password.", "Password must
 * be at least 12 characters long. ..." — so it is shown as-is rather than being replaced
 * with a client-side guess at what went wrong. Only a failure that produced no problem
 * document at all (offline, a proxy 502) gets a message invented here.
 */
export function authErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.problem) return error.problem.detail
    if (error.status === 0) {
      return 'Could not reach Atlas. Check that the server is running and try again.'
    }
    return error.message
  }
  return 'Something went wrong. Please try again.'
}
