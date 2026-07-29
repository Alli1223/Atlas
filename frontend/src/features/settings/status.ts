import type { LozengeAppearance } from '@/components/ui'

import type { Credential, PillStatus, Provider } from './api'

/**
 * The pill status → Lozenge appearance map, and the requested colour contract:
 *
 * | status    | colour | Lozenge appearance |
 * |-----------|--------|--------------------|
 * | valid     | green  | success (lime)     |
 * | expiring  | yellow | moved (warning)    |
 * | expired   | red    | removed (danger)   |
 * | invalid   | red    | removed (danger)   |
 * | unchecked | grey   | default (neutral)  |
 *
 * The table is the single source of truth: the pill component and its test both read it,
 * so the colour of a status is asserted in exactly one place. Reaching for a colour token
 * directly in the component instead is how `invalid` and `expired` would silently drift
 * apart.
 */
export const STATUS_APPEARANCE: Record<PillStatus, LozengeAppearance> = {
  valid: 'success',
  expiring: 'moved',
  expired: 'removed',
  invalid: 'removed',
  unchecked: 'default',
}

/** The short, shouty pill label for each status. */
export const STATUS_LABEL: Record<PillStatus, string> = {
  valid: 'Valid',
  expiring: 'Expiring',
  expired: 'Expired',
  invalid: 'Invalid',
  unchecked: 'Unchecked',
}

/**
 * The statuses that mean "a human needs to do something": rotate, re-authenticate, or
 * look at why a probe rejected the key.
 *
 * This predicate is the whole reason the warning banner exists — it is what decides
 * whether the banner shows at all, and it is deliberately a named export so the banner and
 * its test agree on the exact set. `valid` and `unchecked` are calm states: a key that has
 * simply never been probed is not *wrong*, it is just unverified, and nagging about it
 * would train the user to ignore the banner that matters.
 */
export function needsAttention(status: PillStatus): boolean {
  return status === 'expiring' || status === 'expired' || status === 'invalid'
}

/** The credentials that need attention, in the order the API returned them. */
export function attentionCredentials(credentials: readonly Credential[]): Credential[] {
  return credentials.filter((credential) => needsAttention(credential.status))
}

/** Static, human-facing metadata for each provider. */
export interface ProviderMeta {
  /** The display name shown as the section heading. */
  name: string
  /** One line on what the key unlocks. */
  blurb: string
  /** The kind of secret, for the add-key dialog's label and field. */
  secretNoun: string
  /** A shape hint for the add-key field's placeholder — never a real key. */
  placeholder: string
}

/**
 * The four providers Atlas integrates with, in the order they are shown.
 *
 * A fixed list rather than one derived from the stored credentials: a provider with no key
 * yet must still appear, as a "not configured" row the user can add one to. Deriving the
 * list from the rows would hide exactly the providers a first-time user most needs to see.
 */
export const PROVIDERS: readonly Provider[] = ['github', 'anthropic', 'gemini', 'smtp']

export const PROVIDER_META: Record<Provider, ProviderMeta> = {
  github: {
    name: 'GitHub',
    blurb: 'Link repositories, create branches from cards, and receive webhook events.',
    secretNoun: 'personal access token',
    placeholder: 'ghp_…',
  },
  anthropic: {
    name: 'Anthropic (Claude)',
    blurb: 'Run Claude Code sessions against a card and stream the transcript back.',
    secretNoun: 'API key',
    placeholder: 'sk-ant-…',
  },
  gemini: {
    name: 'Google Gemini',
    blurb: 'Generate project cover art and card reference images.',
    secretNoun: 'API key',
    placeholder: 'AIza…',
  },
  smtp: {
    name: 'SMTP',
    blurb: 'Send email notifications for assignments, mentions, and comments.',
    secretNoun: 'password',
    placeholder: '••••••••',
  },
}

/** Groups credentials under their provider, preserving API order within each group. */
export function groupByProvider(
  credentials: readonly Credential[],
): Record<Provider, Credential[]> {
  const groups = {
    github: [],
    anthropic: [],
    gemini: [],
    smtp: [],
  } as Record<Provider, Credential[]>

  for (const credential of credentials) {
    groups[credential.provider].push(credential)
  }

  return groups
}
