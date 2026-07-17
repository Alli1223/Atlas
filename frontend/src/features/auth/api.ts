import { api, ApiError, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** The signed-in user, as the API describes them. Mirrors `crate::auth::user::UserDto`. */
export type User = components['schemas']['UserDto']

/** A session row. Mirrors `crate::auth::session::SessionDto`. */
export type Session = components['schemas']['SessionDto']

/** Instance-wide role. Mirrors `crate::auth::role::Role`. */
export type Role = components['schemas']['Role']

export interface LoginInput {
  username: string
  password: string
}

export interface ChangePasswordInput {
  currentPassword: string
  newPassword: string
}

/**
 * Fetches the signed-in user, or `null` when there is no session.
 *
 * A 401 is not an error here, it is the answer: "nobody is signed in" is the normal state
 * of the login screen, and modelling it as a rejected query would make every consumer
 * check `isError` and then re-inspect the status to tell "logged out" from "the server is
 * down". Any *other* failure still throws, so a 500 cannot masquerade as a clean logout.
 *
 * This route stays reachable while `mustChangePassword` is set, which is what lets the
 * client discover *why* it is being blocked rather than showing a generic error page.
 */
export async function fetchMe(): Promise<User | null> {
  try {
    return unwrap(await api.GET('/api/v1/auth/me'))
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) return null
    throw error
  }
}

/** Signs in. The session cookie arrives as HttpOnly `Set-Cookie` — unreadable from here. */
export async function login(input: LoginInput): Promise<User> {
  return unwrap(await api.POST('/api/v1/auth/login', { body: input }))
}

/** Signs out. Idempotent: a 204 either way, even with no session. */
export async function logout(): Promise<void> {
  unwrap(await api.POST('/api/v1/auth/logout'))
}

/**
 * Changes the password and rotates the session.
 *
 * The response carries a fresh cookie: the backend revokes *every* session for the user
 * (this one included) and issues a new one, so no reload or re-login is needed here — but
 * every other device is now signed out, by design.
 */
export async function changePassword(input: ChangePasswordInput): Promise<User> {
  return unwrap(await api.POST('/api/v1/auth/change-password', { body: input }))
}

/** The signed-in user's own sessions, newest first. */
export async function fetchSessions(): Promise<Session[]> {
  return unwrap(await api.GET('/api/v1/auth/sessions'))
}

/** Revokes one of the signed-in user's sessions. 404 for a session that is not theirs. */
export async function revokeSession(id: string): Promise<void> {
  unwrap(await api.DELETE('/api/v1/auth/sessions/{id}', { params: { path: { id } } }))
}
