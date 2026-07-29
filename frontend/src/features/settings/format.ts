/** Small date/label formatting for the integrations screen. */

/** A date only, e.g. `16 Jul 2026`. Returns the raw string if it will not parse. */
export function formatDate(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })
}

/**
 * A relative timestamp — `just now`, `5m ago`, `3d ago` — for "last checked".
 *
 * Coarse on purpose: past a week it falls back to the absolute date, because "37d ago" is
 * less useful than the date it names. The absolute date still shows on hover (the caller
 * puts it in a `title`).
 */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return iso
  const seconds = Math.round((now - then) / 1000)

  if (seconds < 45) return 'just now'
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.round(hours / 24)
  if (days <= 7) return `${days}d ago`
  return formatDate(iso)
}

/**
 * How an expiry reads next to the pill: `expires in 3 days`, `expires today`, or
 * `expired 2 days ago`. Whole days, because an hours-precise countdown on a credential is
 * noise — the warning window is measured in days.
 *
 * `null` when there is no known expiry: the backend models a missing expiry header as
 * *unknown*, never as "never expires" (corrections.md #5), so the UI says nothing rather
 * than claiming a key is eternal.
 */
export function expiryPhrase(iso: string | null | undefined, now: number = Date.now()): string | null {
  if (iso === null || iso === undefined) return null
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return null

  const msPerDay = 86_400_000
  const days = Math.round((then - now) / msPerDay)

  if (days > 1) return `expires in ${days} days`
  if (days === 1) return 'expires tomorrow'
  if (days === 0) return 'expires today'
  if (days === -1) return 'expired yesterday'
  return `expired ${Math.abs(days)} days ago`
}
