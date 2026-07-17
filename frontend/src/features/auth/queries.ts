import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as authApi from './api'
import type { ChangePasswordInput, LoginInput, User } from './api'

/**
 * Query keys for everything auth.
 *
 * A single object rather than scattered string literals: `invalidateQueries` and
 * `useQuery` must agree exactly, and a typo in one of them fails *silently* — the screen
 * simply keeps showing stale data.
 */
export const authKeys = {
  all: ['auth'] as const,
  me: () => [...authKeys.all, 'me'] as const,
  sessions: () => [...authKeys.all, 'sessions'] as const,
}

/**
 * The signed-in user, or `null`.
 *
 * Shared options rather than a bare hook so a route loader can `ensureQueryData` the same
 * entry the component reads — the reason the QueryClient is in the router context at all.
 *
 * `staleTime: 0`: auth state is the one thing that must never be served stale. A role
 * change or a deactivation takes effect on the backend's next request (the user is loaded
 * fresh per request), and a cached `/me` would leave the UI showing the old role until the
 * global 30s staleTime elapsed.
 */
export function meQueryOptions() {
  return queryOptions({
    queryKey: authKeys.me(),
    queryFn: authApi.fetchMe,
    staleTime: 0,
    // A 401 is a *value* here (`null`), not an error, so a retry can only ever be a real
    // failure being retried — which is what the global retry is for. Nothing to override.
  })
}

/**
 * The signed-in user.
 *
 * Cookies are HttpOnly, so the frontend cannot read the session and there is nothing to
 * derive auth state *from* except the server's answer. This hook is that answer:
 *
 * - `user === null` with `isPending === false` → nobody is signed in.
 * - `user.mustChangePassword` → the account is locked to the change-password screen.
 * - `isError` → the server could not be reached; that is NOT the same as logged out, and
 *   the guard deliberately does not redirect on it.
 */
export function useCurrentUser() {
  const query = useQuery(meQueryOptions())

  return {
    /** The signed-in user, or `null` when signed out. `undefined` while first loading. */
    user: query.data,
    /** True until the first answer arrives. Render a loading state, not the login screen. */
    isPending: query.isPending,
    /** The `/me` call itself failed — the server is unreachable or broken. */
    isError: query.isError,
    error: query.error as ApiError | null,
    /** True once an answer exists and it names a user. */
    isAuthenticated: query.data != null,
    refetch: query.refetch,
  }
}

/**
 * Signs in, then seeds the `/me` cache with the user the login returned.
 *
 * `setQueryData` rather than `invalidateQueries`: the login response *is* a fresh UserDto
 * from the same server, so refetching it immediately would be a second round-trip for an
 * answer already in hand — and it would race the redirect, which reads `/me` to decide
 * where to send the user.
 */
export function useLogin() {
  const queryClient = useQueryClient()

  return useMutation<User, ApiError, LoginInput>({
    mutationFn: authApi.login,
    onSuccess: (user) => {
      queryClient.setQueryData(authKeys.me(), user)
    },
  })
}

/**
 * Signs out and clears the cache.
 *
 * `clear()` rather than invalidating `/me`: everything cached — boards, cards, user lists —
 * was fetched as *that* user. Leaving it in place means the next person to sign in on this
 * browser sees a flash of the previous user's data before the refetches land.
 */
export function useLogout() {
  const queryClient = useQueryClient()

  return useMutation<void, ApiError, void>({
    mutationFn: authApi.logout,
    onSuccess: () => {
      queryClient.clear()
    },
  })
}

/**
 * Changes the password.
 *
 * The response carries the updated user (with `mustChangePassword` cleared) and a rotated
 * cookie, so seeding `/me` here is what releases the forced-reset gate — the guard is
 * watching that exact cache entry.
 *
 * The rest of the cache is dropped: every other session for this user was just revoked, so
 * anything fetched before this point belongs to a session that no longer exists.
 */
export function useChangePassword() {
  const queryClient = useQueryClient()

  return useMutation<User, ApiError, ChangePasswordInput>({
    mutationFn: authApi.changePassword,
    onSuccess: (user) => {
      queryClient.removeQueries()
      queryClient.setQueryData(authKeys.me(), user)
    },
  })
}

/** The signed-in user's own sessions. */
export function useSessions() {
  return useQuery({
    queryKey: authKeys.sessions(),
    queryFn: authApi.fetchSessions,
  })
}
