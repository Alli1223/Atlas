import { type QueryClient } from '@tanstack/react-query'
import { createRootRouteWithContext, Outlet } from '@tanstack/react-router'
import { lazy, Suspense } from 'react'

import { AuthGate } from '@/features/auth'

/** Router context. The QueryClient is injected here so route loaders and components
 *  share one cache — `context.queryClient.ensureQueryData(...)` in a loader then warms
 *  the same cache the component reads. */
export interface RouterContext {
  queryClient: QueryClient
}

// Devtools are dev-only and lazily loaded, so they never reach the production bundle.
const TanStackRouterDevtools = import.meta.env.DEV
  ? lazy(() =>
      import('@tanstack/react-router-devtools').then((m) => ({
        default: m.TanStackRouterDevtools,
      })),
    )
  : () => null

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootComponent,
})

function RootComponent() {
  return (
    <>
      {/* Every route in Atlas is behind the guard, and the guard owns the shell rather than
          sitting inside it: the signed-out screens must render with no nav, no search and no
          avatar, because all three need a session. Wrapping here rather than per-route is
          what makes it impossible for a new route to forget — the failure mode of an opt-in
          guard is silent, and it is a page that quietly serves an unauthenticated user. */}
      <AuthGate>
        <Outlet />
      </AuthGate>
      <Suspense fallback={null}>
        <TanStackRouterDevtools position="bottom-right" />
      </Suspense>
    </>
  )
}
