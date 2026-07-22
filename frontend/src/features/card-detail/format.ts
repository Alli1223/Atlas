/** Small date/label formatting shared across the card view. */

/** An absolute, human date-time, e.g. `16 Jul 2026, 14:32`. */
export function formatDateTime(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** A date only, e.g. `16 Jul 2026`. */
export function formatDate(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })
}

/**
 * A relative timestamp — `just now`, `5m ago`, `3d ago` — for comment and history rows.
 *
 * Coarse on purpose: past a week it falls back to the absolute date, because "37d ago" is
 * less useful than the date it names. The absolute time still shows on hover (the caller
 * puts it in a `title`), which is the ADS pattern.
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
