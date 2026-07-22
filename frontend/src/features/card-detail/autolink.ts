/**
 * Card-key autolinking: turning `ATLAS-123` in free text into a live link.
 *
 * This is the ⭐ item from TODO.md Phase 9 — a mention of a card key, in a comment or a
 * description or a commit-shaped note, becomes a link to that card. It is a *rendering*
 * concern, never a storage one: the markdown source keeps `ATLAS-123` as plain text, and
 * the link is synthesised at read time. Baking a link into the stored markdown would break
 * the editor round-trip (`ATLAS-123` would come back as `[ATLAS-123](/cards/ATLAS-123)`)
 * and would rot the moment a card is renumbered.
 *
 * Kept deliberately small and dependency-free so the board agent can reuse it for card
 * titles and tooltips without pulling in the rest of card-detail.
 */

/**
 * A card key, e.g. `ATLAS-123`.
 *
 * The grammar mirrors the backend's: a project key is uppercase ASCII letters/digits
 * starting with a letter (see `crate::domain::project`), then a hyphen, then the per-project
 * counter. `\d+` not `\d{1,n}` — counters are never reused and there is no ceiling.
 */
const CARD_KEY = /[A-Z][A-Z0-9]*-\d+/g

/** A segment of text: either a plain run or a recognised card key. */
export type TextSegment =
  | { kind: 'text'; text: string }
  | { kind: 'card-key'; key: string; text: string }

/**
 * Splits a string into plain-text runs and card-key matches.
 *
 * A boundary check on both sides keeps `ATLAS-12` out of `ATLAS-123` and stops a key being
 * found inside a larger token like `NOTATLAS-1` or `ATLAS-1X` — the match must be flanked by
 * a non-`[A-Za-z0-9-]` character or an edge. `String.matchAll` with the boundary re-checked
 * by hand is more predictable here than lookarounds, which browsers only fully settled
 * recently.
 */
export function splitCardKeys(text: string): TextSegment[] {
  const segments: TextSegment[] = []
  let cursor = 0

  for (const match of text.matchAll(CARD_KEY)) {
    const start = match.index
    const key = match[0]
    const end = start + key.length

    const before = start === 0 ? '' : (text[start - 1] ?? '')
    const after = end >= text.length ? '' : (text[end] ?? '')
    // Reject a match welded to an identifier character on either flank.
    if (isKeyAdjacent(before) || isKeyAdjacent(after)) continue

    if (start > cursor) segments.push({ kind: 'text', text: text.slice(cursor, start) })
    segments.push({ kind: 'card-key', key, text: key })
    cursor = end
  }

  if (cursor < text.length) segments.push({ kind: 'text', text: text.slice(cursor) })
  // An all-plain string still yields one segment, so callers never special-case "no match".
  if (segments.length === 0) segments.push({ kind: 'text', text })
  return segments
}

/** Whether a character would make a card key part of a larger token. */
function isKeyAdjacent(char: string): boolean {
  return char !== '' && /[A-Za-z0-9-]/.test(char)
}

/** The in-app path a card key links to. */
export function cardHref(key: string): string {
  return `/cards/${key}`
}

/** Whether a whole string is exactly one card key and nothing else. */
export function isCardKey(text: string): boolean {
  const single = /^[A-Z][A-Z0-9]*-\d+$/
  return single.test(text)
}
