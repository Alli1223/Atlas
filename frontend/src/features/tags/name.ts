/**
 * The tag-name rule, client side.
 *
 * # Why this exists when the backend already enforces it
 *
 * It does *not* exist to protect the database — the backend's `domain::tag::validate_name`
 * does that, and it is the only thing that can, since this code runs on a machine the user
 * controls. This exists so the picker can say "no spaces — try `needs-review`" while the
 * user is still typing it, rather than after a round trip that returns a 422.
 *
 * That makes the duplication deliberate but load-bearing: the two must agree, or the
 * client refuses names the server would take (silently narrower) or promises names it
 * would reject (a 422 the user was told would not happen). `name.test.ts` pins the rule
 * against the same cases `domain::tag`'s unit tests use.
 */

/** Longest accepted tag name, in characters. Mirrors `domain::tag::MAX_NAME`. */
export const MAX_TAG_NAME = 50

/** Why a tag name was refused, or `null` if it was not. */
export type TagNameError = 'empty' | 'whitespace' | 'too-long' | 'control'

/** C0 and C1 control characters, plus DEL. Mirrors Rust's `char::is_control`. */
// eslint-disable-next-line no-control-regex -- matching control characters is the point
const CONTROL = /[\u0000-\u001F\u007F-\u009F]/u

/** Any Unicode whitespace, not merely the ASCII space. See [`validateTagName`]. */
const WHITESPACE = /\s/u

/**
 * `needs review` → `needs-review`.
 *
 * Offered to the user, never applied for them: silently rewriting what someone typed is
 * how you end up with tags nobody meant to create.
 */
export function hyphenate(name: string): string {
  return name.trim().split(WHITESPACE_RUN).filter(Boolean).join('-')
}

const WHITESPACE_RUN = /\s+/u

/**
 * Checks a tag name the way the backend does.
 *
 * The whitespace test is `\s`, not `' '`. A tab, a newline and above all a non-breaking
 * space (U+00A0) break the future query grammar exactly as a space does, and U+00A0 is the
 * one people paste without ever seeing it — invisible in every UI, and waved straight
 * through by a check for the ASCII space alone.
 */
export function validateTagName(raw: string): TagNameError | null {
  const name = raw.trim()

  if (name.length === 0) return 'empty'
  // Whitespace BEFORE control, matching `domain::tag::validate_name` — and the order is
  // the difference between a useful message and a baffling one. A tab and a newline are
  // both control characters and whitespace; check control first and `a\tb` is refused as
  // "contains control characters", which is true, unhelpful, and wrong about what the
  // user did. They pasted out of a spreadsheet. To them it is a space, and the fix is
  // `a-b`. NUL and DEL are not whitespace, so they still fall through and are still
  // named accurately.
  if (WHITESPACE.test(name)) return 'whitespace'
  if (CONTROL.test(name)) return 'control'
  // Spread, not `.length`: a name of astral-plane characters (an emoji tag) would
  // otherwise count double against a limit the backend counts in `char`s.
  if ([...name].length > MAX_TAG_NAME) return 'too-long'

  return null
}

/** The message to show for a refused name. Written for the person who typed it. */
export function tagNameErrorMessage(error: TagNameError, raw: string): string {
  switch (error) {
    case 'empty':
      return 'Enter a tag name.'
    case 'whitespace':
      return `Tag names cannot contain spaces. Try “${hyphenate(raw)}”.`
    case 'too-long':
      return `Tag names must be ${MAX_TAG_NAME} characters or fewer.`
    case 'control':
      return 'That name contains characters a tag cannot hold.'
  }
}

/** Whether a name is usable as-is. */
export function isValidTagName(raw: string): boolean {
  return validateTagName(raw) === null
}

/**
 * Ranks tags for the picker's autocomplete.
 *
 * Case-insensitive, because the backend's names are `COLLATE NOCASE` — a picker that hid
 * `Bug` when you typed `b` would be offering to create a duplicate the server then refuses
 * with a 409.
 *
 * Prefix matches rank above substring matches: someone typing `re` means `refactor` or
 * `reference` far more often than `breaking-change`, and a picker whose first row is not
 * the obvious one is a picker people stop reading.
 */
export function rankTags<T extends { name: string }>(tags: readonly T[], query: string): T[] {
  const q = query.trim().toLowerCase()
  if (q.length === 0) return [...tags]

  const scored: { tag: T; rank: number }[] = []
  for (const tag of tags) {
    const name = tag.name.toLowerCase()
    if (name.startsWith(q)) scored.push({ tag, rank: 0 })
    else if (name.includes(q)) scored.push({ tag, rank: 1 })
  }

  // Stable within a rank: the server already sorted by name, and re-sorting inside a band
  // would throw that away for no reason.
  return scored.sort((a, b) => a.rank - b.rank).map((s) => s.tag)
}
