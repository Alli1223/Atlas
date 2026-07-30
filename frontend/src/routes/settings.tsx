import { createFileRoute } from '@tanstack/react-router'

import { credentialsQueryOptions, IntegrationsPage } from '@/features/settings'

/**
 * Settings → Integrations.
 *
 * The loader warms the credentials cache so the page paints its provider list without a
 * spinner on navigation. It deliberately does **not** `throw` on a 403: a non-admin lands
 * on the page and reads *why* they cannot manage keys, rather than hitting a generic error
 * boundary — `IntegrationsPage` renders that explanation from the query's error state. The
 * `.catch` keeps the loader from rejecting for exactly that reason.
 */
export const Route = createFileRoute('/settings')({
  loader: ({ context }) =>
    context.queryClient.ensureQueryData(credentialsQueryOptions()).catch(() => undefined),
  component: IntegrationsPage,
})
