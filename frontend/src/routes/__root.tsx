import { type QueryClient } from '@tanstack/react-query'
import { createRootRouteWithContext, Outlet } from '@tanstack/react-router'
import { lazy, Suspense } from 'react'

import { AppShell } from '@/components/shell/AppShell'

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
      <AppShell>
        <Outlet />
      </AppShell>
      <Suspense fallback={null}>
        <TanStackRouterDevtools position="bottom-right" />
      </Suspense>
    </>
  )
}
