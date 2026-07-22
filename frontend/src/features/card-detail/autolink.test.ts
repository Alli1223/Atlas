import { describe, expect, it } from 'vitest'

import { cardHref, isCardKey, splitCardKeys } from './autolink'

describe('splitCardKeys', () => {
  it('finds a bare card key and splits the text around it', () => {
    const parts = splitCardKeys('blocked by ATLAS-42 for now')
    expect(parts).toEqual([
      { kind: 'text', text: 'blocked by ' },
      { kind: 'card-key', key: 'ATLAS-42', text: 'ATLAS-42' },
      { kind: 'text', text: ' for now' },
    ])
  })

  it('finds several keys in one string', () => {
    const parts = splitCardKeys('ATLAS-1 relates to WEB-200')
    const keys = parts.filter((p) => p.kind === 'card-key').map((p) => p.text)
    expect(keys).toEqual(['ATLAS-1', 'WEB-200'])
  })

  it('does not match a key welded to surrounding identifier characters', () => {
    // Guards against false links inside larger tokens and hyphenated words. (`NOTATLAS-1`
    // is deliberately NOT here: it is a legitimate standalone key for a project keyed
    // NOTATLAS — the guard is about a key fused to *adjacent* characters, not a longer key.)
    for (const text of ['ATLAS-1X', 'super-ATLAS-1', 'ATLAS-1-beta', 'xATLAS-1']) {
      expect(splitCardKeys(text).some((p) => p.kind === 'card-key')).toBe(false)
    }
  })

  it('does not treat a lowercase or numeric-leading token as a key', () => {
    expect(splitCardKeys('atlas-1 and 4-5').every((p) => p.kind === 'text')).toBe(true)
  })

  it('returns a single text segment for plain input so callers never special-case no-match', () => {
    expect(splitCardKeys('nothing here')).toEqual([{ kind: 'text', text: 'nothing here' }])
  })

  it('links to the in-app card route', () => {
    expect(cardHref('ATLAS-42')).toBe('/cards/ATLAS-42')
  })

  it('recognises a whole-string key', () => {
    expect(isCardKey('ATLAS-42')).toBe(true)
    expect(isCardKey('ATLAS-42 ')).toBe(false)
    expect(isCardKey('see ATLAS-42')).toBe(false)
  })
})
