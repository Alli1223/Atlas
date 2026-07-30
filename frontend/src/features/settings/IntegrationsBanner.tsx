import { type ReactNode } from 'react'

import { Banner } from '@/components/ui'

import type { Credential } from './api'
import { useCredentials } from './queries'
import { attentionCredentials, PROVIDER_META } from './status'

/**
 * Builds the one-line summary. `expired`/`invalid` are the loud states, so the count of
 * those drives the wording; a lone `expiring` key is a gentler "needs renewing soon".
 */
function summarise(attention: readonly Credential[]): string {
  if (attention.length === 1) {
    const only = attention[0]!
    const provider = PROVIDER_META[only.provider].name
    const verb =
      only.status === 'expiring'
        ? 'is expiring soon'
        : only.status === 'expired'
          ? 'has expired'
          : 'was rejected'
    return `The ${provider} key “${only.label}” ${verb}. Open it below to renew or replace it.`
  }

  const broken = attention.filter((c) => c.status !== 'expiring').length
  const lead = `${attention.length} integration keys need attention`
  return broken > 0
    ? `${lead} — ${broken} expired or invalid. Renew or replace them below.`
    : `${lead}. Renew them below before they expire.`
}

export interface IntegrationsBannerProps {
  /** Trailing controls — e.g. a "Review" link when the banner is mounted app-wide. */
  actions?: ReactNode
}

/**
 * The standing warning about API keys that need a human.
 *
 * This is the warning Alastair asked for. It reads the credentials cache itself rather
 * than taking them as a prop, so it is drop-in anywhere — the settings page renders it
 * today, and the app shell can mount the exact same component tomorrow to surface an
 * expired PAT from any screen. TanStack Query dedupes the fetch by key, so a second mount
 * costs nothing.
 *
 * It renders **only** when something genuinely needs attention (`expiring`, `expired`, or
 * `invalid`) — never for a healthy or merely-unchecked instance. That restraint is what
 * keeps it worth reading: a banner that is always present is a banner nobody sees. While
 * the list is loading, or if the fetch failed, it stays silent rather than flashing a
 * warning it cannot yet justify.
 *
 * `error` appearance when any key is outright expired or invalid (that interrupts, via
 * `role="alert"`); `warning` when the worst case is merely expiring soon.
 */
export function IntegrationsBanner({ actions }: IntegrationsBannerProps) {
  const { data: credentials } = useCredentials()

  if (credentials === undefined) return null

  const attention = attentionCredentials(credentials)
  if (attention.length === 0) return null

  const hasBroken = attention.some((credential) => credential.status !== 'expiring')

  return (
    <Banner appearance={hasBroken ? 'error' : 'warning'} {...(actions !== undefined ? { actions } : {})}>
      {summarise(attention)}
    </Banner>
  )
}
