import { type QueryClient } from '@tanstack/react-query'
import { createRouter, type RouterHistory } from '@tanstack/react-router'

import { routeTree } from './routeTree.gen'

/**
 * Builds the app router.
 *
 * A factory rather than a module-level singleton so tests can mount the real route tree
 * with a memory history and get the *same* router type — the ambient `Register`
 * declaration below pins that type globally, so a router built any other way would not
 * satisfy `RouterProvider` and would need an `as any` at every call site.
 */
export function createAppRouter(queryClient: QueryClient, history?: RouterHistory) {
  return createRouter({
    routeTree,
    // Wiring the QueryClient into router context means loaders and components share one
    // cache: `context.queryClient.ensureQueryData(...)` in a loader warms the same cache
    // the component reads.
    context: { queryClient },
    defaultPreload: 'intent',
    scrollRestoration: true,
    ...(history ? { history } : {}),
  })
}

export type AppRouter = ReturnType<typeof createAppRouter>

declare module '@tanstack/react-router' {
  interface Register {
    router: AppRouter
  }
}
