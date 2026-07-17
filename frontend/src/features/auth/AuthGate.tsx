import { useQueryClient } from '@tanstack/react-query'
import { Navigate, useNavigate, useRouterState } from '@tanstack/react-router'
import { type ReactNode, useEffect } from 'react'

import { AppShell } from '@/components/shell/AppShell'
import { Button, Spinner } from '@/components/ui'

import styles from './AuthGate.module.css'
import { authErrorMessage, isPasswordChangeRequired } from './problem'
import { authKeys, useCurrentUser } from './queries'

/** The route that collects credentials. */
export const LOGIN_ROUTE = '/login'

/** The route the forced-reset gate allows, and the only one. */
export const CHANGE_PASSWORD_ROUTE = '/change-password'

/**
 * Routes that render without a session.
 *
 * Only the style guide. It is developer documentation — a static rendering of the design
 * system's primitives, holding no data of any kind — and gating it costs two real things:
 * you could not look at the design system without an account, and `e2e/contrast.spec.ts`
 * (which measures computed colour on that page, and has never needed a backend) would have
 * to grow a login just to reach it.
 *
 * This is not a hole in the gate. A *signed-in* user still gets the shell here, and a user
 * who owes a password change is still bounced to the reset screen from here like anywhere
 * else — the forced-reset check runs before this one.
 */
const PUBLIC_ROUTES: ReadonlySet<string> = new Set(['/styleguide'])

/**
 * Where to send a user after a successful sign-in, given a `redirect` search param.
 *
 * Only same-site absolute paths survive. `redirect` is attacker-controllable — it is right
 * there in the URL — so without this check `…/login?redirect=https://evil.example` turns
 * Atlas's own login screen into a credible phishing hop: the victim signs in to the real
 * Atlas and is then handed to the attacker's page, having watched the real domain in the
 * address bar the whole way.
 *
 * `//evil.example` is rejected too. It looks relative and is not: browsers read a
 * protocol-relative URL as a full origin, which is the exact form a `startsWith('/')` check
 * misses.
 */
export function safeRedirect(target: string | undefined): string {
  if (target === undefined || target === '') return '/'
  if (!target.startsWith('/')) return '/'
  if (target.startsWith('//')) return '/'
  // `/\evil.example` — some browsers normalise the backslash to a forward slash and it
  // becomes protocol-relative again.
  if (target.startsWith('/\\')) return '/'
  return target
}

/**
 * Navigates to a same-site href that was built at runtime.
 *
 * TanStack Router's `to` is typed against the generated route literals, which is the point
 * of it — but a post-login redirect target is a *string from the URL bar*, known only at
 * runtime, and no route literal describes it. `navigate({ href })` is the router's own
 * escape hatch for that: it parses the href back into `to`/`search`/`hash` and performs an
 * ordinary client-side navigation, so this is not a page load.
 *
 * The `<Navigate>` component would do the same job, but its generics make `to` mandatory
 * when `href` is the only destination given, so it cannot express this without inline type
 * arguments at every call site.
 *
 * **`href` must already have been through [`safeRedirect`].** Nothing here re-checks it.
 */
export function NavigateToHref({ href, replace = true }: { href: string; replace?: boolean }) {
  const navigate = useNavigate()

  useEffect(() => {
    void navigate({ href, replace })
  }, [navigate, href, replace])

  return null
}

function FullPageSpinner({ label }: { label: string }) {
  return (
    <div className={styles.centre}>
      <Spinner size="large" label={label} />
    </div>
  )
}

/**
 * The auth guard.
 *
 * # The rules, in order
 *
 * 1. **Loading** — render a spinner. Not the login screen: a flash of "sign in" for a user
 *    who *is* signed in is both wrong and alarming.
 * 2. **`/me` failed** — render an error with a retry. Deliberately NOT a redirect to login:
 *    an unreachable server is not a signed-out user, and treating it as one would silently
 *    log everybody out every time the backend restarts.
 * 3. **No session** — go to `/login`, remembering where the user was headed.
 * 4. **`mustChangePassword`** — go to `/change-password` and stay there. No shell is
 *    rendered, so there is nothing to click past; any other path bounces straight back.
 * 5. Otherwise — render the app inside its shell.
 *
 * # Why a component and not `beforeLoad`
 *
 * Auth state lives in the `/me` query, and TanStack Query owns its lifecycle: the cache is
 * what login seeds, logout clears, and change-password updates. A `beforeLoad` guard would
 * have to re-derive that state per navigation and could not react when it changes *without*
 * one — which is exactly what happens when a session is revoked from another device.
 */
export function AuthGate({ children }: { children: ReactNode }) {
  const { user, isPending, isError, error, refetch } = useCurrentUser()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const href = useRouterState({ select: (state) => state.location.href })
  const queryClient = useQueryClient()

  // The forced-reset gate can close underneath a session that was fine a moment ago — an
  // admin resets an account while its tab is open. The backend says so with a
  // machine-readable marker on the 403, so any request carrying it means `/me` is stale;
  // refetching it lets the rules above do the rest.
  //
  // The marker, never the message: the same 403 status also means "you are not an admin",
  // and the only thing that separates them is the `type` URN.
  useEffect(() => {
    const onError = (candidate: unknown) => {
      if (isPasswordChangeRequired(candidate)) {
        void queryClient.invalidateQueries({ queryKey: authKeys.me() })
      }
    }

    const unsubscribeQueries = queryClient.getQueryCache().subscribe((event) => {
      if (event.type === 'updated' && event.action.type === 'error') {
        onError(event.action.error)
      }
    })
    const unsubscribeMutations = queryClient.getMutationCache().subscribe((event) => {
      if (event.type === 'updated' && event.action.type === 'error') {
        onError(event.action.error)
      }
    })

    return () => {
      unsubscribeQueries()
      unsubscribeMutations()
    }
  }, [queryClient])

  const isPublic = PUBLIC_ROUTES.has(pathname)
  const isAuthRoute = pathname === LOGIN_ROUTE || pathname === CHANGE_PASSWORD_ROUTE

  if (isPending) {
    return <FullPageSpinner label="Signing you in" />
  }

  if (isError) {
    // A public route needs no session, so a dead server is no reason not to render it.
    if (isPublic) return <>{children}</>

    return (
      <div className={styles.centre}>
        <div className={styles.message}>
          <h1 className={styles.title}>Atlas is not responding</h1>
          <p className={styles.detail}>{authErrorMessage(error)}</p>
        </div>
        <Button appearance="primary" onClick={() => void refetch()}>
          Try again
        </Button>
      </div>
    )
  }

  if (user == null) {
    if (pathname === LOGIN_ROUTE || isPublic) return <>{children}</>
    // `href` carries the search and hash too, so a deep link survives the round trip.
    return <Navigate to={LOGIN_ROUTE} search={{ redirect: href }} replace />
  }

  if (user.mustChangePassword && pathname !== CHANGE_PASSWORD_ROUTE) {
    // The no-escape rule. `replace` so Back cannot walk into the app either — it would
    // land on the entry this navigation replaced, and be bounced here again anyway.
    return <Navigate to={CHANGE_PASSWORD_ROUTE} replace />
  }

  // Auth screens render bare: the shell's nav, search and avatar all need a session that,
  // on these two routes, either does not exist or cannot do anything yet.
  if (isAuthRoute) return <>{children}</>

  return <AppShell>{children}</AppShell>
}
