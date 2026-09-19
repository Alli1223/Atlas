/** Small date/label formatting shared across the card view. */

/**
 * The units [`formatMinutes`] breaks a total into, largest first, on the same 5-day / 8-hour
 * working calendar `crate::domain::worklog::duration_token_minutes` parses against — so a
 * duration a user typed in and the one rendered back agree.
 */
const DURATION_UNITS: readonly [unit: string, minutes: number][] = [
  ['w', 5 * 8 * 60],
  ['d', 8 * 60],
  ['h', 60],
  ['m', 1],
]

/** Total minutes as a duration string, e.g. `150` → `"2h 30m"`, `500` → `"1d 20m"`. */
export function formatMinutes(totalMinutes: number): string {
  let remaining = totalMinutes
  const parts: string[] = []
  for (const [unit, perUnit] of DURATION_UNITS) {
    const value = Math.floor(remaining / perUnit)
    if (value > 0) {
      parts.push(`${value}${unit}`)
      remaining -= value * perUnit
    }
  }
  return parts.length > 0 ? parts.join(' ') : '0m'
}

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
