import { describe, expect, it } from 'vitest'

import {
  hyphenate,
  isValidTagName,
  MAX_TAG_NAME,
  rankTags,
  tagNameErrorMessage,
  validateTagName,
} from './name'

/**
 * These mirror `domain::tag`'s unit tests case for case, deliberately.
 *
 * The client rule exists only to say "no spaces" before the round trip; if the two ever
 * disagree, the picker either refuses names the server would take or promises names it
 * rejects. Same cases on both sides is the cheapest way to notice.
 */
describe('validateTagName', () => {
  it('accepts an ordinary tag', () => {
    expect(validateTagName('good-first-issue')).toBeNull()
    expect(isValidTagName('good-first-issue')).toBe(true)
  })

  it('rejects a name with a space', () => {
    expect(validateTagName('needs review')).toBe('whitespace')
  })

  it.each([
    ['non-breaking space', 'a\u00A0b'],
    ['tab', 'a\tb'],
    ['newline', 'a\nb'],
    ['figure space', 'a\u2007b'],
    ['ideographic space', 'a\u3000b'],
  ])('rejects a %s, not just the ASCII space', (_label, name) => {
    // U+00A0 is the one that matters: pasted rather than typed, invisible in every UI,
    // and it breaks the future query grammar exactly as a plain space does. A `=== ' '`
    // check would wave it straight through.
    expect(validateTagName(name)).toBe('whitespace')
  })

  it('trims surrounding whitespace rather than rejecting it', () => {
    // Leading/trailing space is a typing artefact, not an ambiguous name.
    expect(validateTagName('  bug  ')).toBeNull()
    expect(validateTagName('\tbug\n')).toBeNull()
  })

  it('rejects an empty or whitespace-only name', () => {
    expect(validateTagName('')).toBe('empty')
    expect(validateTagName('   ')).toBe('empty')
  })

  it('rejects control characters', () => {
    expect(validateTagName('a\u0000b')).toBe('control')
    expect(validateTagName('a\u001Bb')).toBe('control')
    expect(validateTagName('a\u007Fb')).toBe('control')
  })

  it.each(['c++', 'i18n', '.NET', '3d-print', 'v1.2.0', '@home', 'a/b'])(
    'allows punctuation: %s',
    (name) => {
      // Only spaces are ambiguous. The rule is not a general sanitiser and must not grow
      // into one — these are tags people mean.
      expect(validateTagName(name)).toBeNull()
    },
  )

  it('caps the length', () => {
    expect(validateTagName('a'.repeat(MAX_TAG_NAME))).toBeNull()
    expect(validateTagName('a'.repeat(MAX_TAG_NAME + 1))).toBe('too-long')
  })

  it('counts characters, not UTF-16 code units', () => {
    // The backend counts `char`s. A picker that counted `.length` would refuse an emoji
    // tag at 25 characters while the server happily took 50.
    const astral = '🎨'.repeat(MAX_TAG_NAME)
    expect(astral.length).toBe(MAX_TAG_NAME * 2)
    expect(validateTagName(astral)).toBeNull()
  })

  it('reports spaces before length, so the message names the real problem', () => {
    // A 60-character name with a space in it is both too long and ambiguous. The space is
    // the fixable one and the one the user meant to type, so it wins.
    expect(validateTagName(`${'a'.repeat(60)} b`)).toBe('whitespace')
  })
})

describe('hyphenate', () => {
  it('joins words with hyphens', () => {
    expect(hyphenate('needs review')).toBe('needs-review')
    expect(hyphenate('  take   home  test ')).toBe('take-home-test')
  })

  it('leaves an already-legal name alone', () => {
    expect(hyphenate('needs-review')).toBe('needs-review')
  })

  it('collapses every kind of whitespace, not just spaces', () => {
    expect(hyphenate('a\u00A0b\tc')).toBe('a-b-c')
  })
})

describe('tagNameErrorMessage', () => {
  it('shows the user what to type instead of a space', () => {
    // A rule nobody was told about is an obstacle; a rule that shows the convention is a
    // convention.
    expect(tagNameErrorMessage('whitespace', 'needs review')).toContain('needs-review')
  })

  it('names the limit rather than just refusing', () => {
    expect(tagNameErrorMessage('too-long', 'x')).toContain(String(MAX_TAG_NAME))
  })
})

describe('rankTags', () => {
  const tags = [
    { name: 'blocked' },
    { name: 'breaking-change' },
    { name: 'reference' },
    { name: 'refactor' },
    { name: 'Bug' },
  ]

  it('returns everything for an empty query', () => {
    expect(rankTags(tags, '')).toHaveLength(5)
    expect(rankTags(tags, '   ')).toHaveLength(5)
  })

  it('ranks prefix matches above substring matches', () => {
    // Someone typing `re` means `refactor` or `reference`, not `breaking-change`. A picker
    // whose first row is not the obvious one is a picker people stop reading.
    const ranked = rankTags(tags, 're').map((t) => t.name)
    expect(ranked.slice(0, 2).sort()).toEqual(['refactor', 'reference'])
    expect(ranked.at(-1)).toBe('breaking-change')
  })

  it('matches case-insensitively', () => {
    // The backend's names are COLLATE NOCASE. A picker that hid `Bug` when you typed `b`
    // would offer to create a duplicate the server then refuses with a 409.
    expect(rankTags(tags, 'bug').map((t) => t.name)).toEqual(['Bug'])
    expect(rankTags(tags, 'BUG').map((t) => t.name)).toEqual(['Bug'])
  })

  it('excludes tags that do not match at all', () => {
    expect(rankTags(tags, 'zzz')).toEqual([])
  })

  it('keeps the server ordering within a rank band', () => {
    // The server already sorted by name; re-sorting inside a band would throw that away.
    expect(rankTags(tags, 'b').map((t) => t.name)).toEqual(['blocked', 'breaking-change', 'Bug'])
  })
})
