/**
 * Minimal class-name joiner. Atlas has no need for `clsx` — this is the whole feature.
 */
export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(' ')
}
