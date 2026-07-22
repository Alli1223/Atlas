import { describe, expect, it } from 'vitest'

import {
  type Doc,
  emptyDoc,
  isBlankMarkdown,
  parseInline,
  parseMarkdown,
  serializeMarkdown,
} from './markdown'

/**
 * The editor stores markdown, but edits a ProseMirror document. These tests pin the two
 * converters that bridge that gap — the exact load (`parseMarkdown`) and save
 * (`serializeMarkdown`) path TipTap runs. A converter that silently drops a construct is the
 * failure mode that matters: it looks fine until someone reloads and their checklist is
 * gone, so every supported construct is asserted to survive the full round trip.
 */

/** The property the editor depends on: source → doc → source is the identity. */
function roundTrip(markdown: string): string {
  return serializeMarkdown(parseMarkdown(markdown))
}

describe('markdown round trip', () => {
  const cases: Record<string, string> = {
    'a plain paragraph': 'Just some text.',
    bold: 'Some **bold** text.',
    italic: 'Some *italic* text.',
    strikethrough: 'Some ~~struck~~ text.',
    'inline code': 'Call `useEditor()` here.',
    'a link': 'See [the docs](https://example.com/x).',
    'a mention': 'Ping @alice about it.',
    'nested emphasis': 'A **bold and *italic* run**.',
    'a heading': '## A section heading',
    'a bullet list': '- first\n- second\n- third',
    'an ordered list': '1. first\n2. second\n3. third',
    'a task list': '- [ ] todo item\n- [x] done item',
    'a blockquote': '> quoted line',
    'a horizontal rule': '---',
  }

  for (const [name, markdown] of Object.entries(cases)) {
    it(`preserves ${name}`, () => {
      // Trailing newline is the canonical serialiser output; the inputs above omit it.
      expect(roundTrip(markdown)).toBe(`${markdown}\n`)
    })
  }

  it('preserves a fenced code block with its language and body verbatim', () => {
    const source = '```ts\nconst x: number = 1\nif (x) return\n```'
    expect(roundTrip(source)).toBe(`${source}\n`)
  })

  it('does not treat snake_case as emphasis', () => {
    // The classic false positive: `_` inside an identifier must not open italics.
    expect(roundTrip('call some_snake_case_name now')).toBe('call some_snake_case_name now\n')
    const nodes = parseInline('some_snake_case_name')
    expect(nodes).toHaveLength(1)
    expect(nodes[0]).toMatchObject({ type: 'text', text: 'some_snake_case_name' })
  })

  it('keeps a card key as literal text so autolinking stays a render concern', () => {
    // If the parser linked ATLAS-1, the source would come back as [ATLAS-1](/cards/ATLAS-1)
    // and the editor would corrupt what the user typed on every save.
    expect(roundTrip('blocks ATLAS-1 and ATLAS-2')).toBe('blocks ATLAS-1 and ATLAS-2\n')
    const nodes = parseInline('blocks ATLAS-1')
    expect(nodes.every((n) => n.type !== 'mention')).toBe(true)
    expect(nodes.some((n) => n.type === 'text' && n.text.includes('ATLAS-1'))).toBe(true)
  })
})

describe('markdown parsing structure', () => {
  it('marks a checked task item as checked and an unchecked one as not', () => {
    const doc = parseMarkdown('- [x] done\n- [ ] todo')
    const list = doc.content[0]
    expect(list?.type).toBe('taskList')
    const items = (list?.content ?? []) as Doc['content']
    expect(items[0]?.attrs).toMatchObject({ checked: true })
    expect(items[1]?.attrs).toMatchObject({ checked: false })
  })

  it('parses a link into a text node carrying a link mark with the href', () => {
    const nodes = parseInline('[label](https://example.com)')
    expect(nodes[0]).toMatchObject({
      type: 'text',
      text: 'label',
      marks: [{ type: 'link', attrs: { href: 'https://example.com' } }],
    })
  })

  it('yields an empty document for blank source rather than an empty content array', () => {
    // ProseMirror's schema requires at least one block; an empty content array crashes TipTap.
    expect(parseMarkdown('')).toEqual(emptyDoc())
    expect(parseMarkdown('   \n  \n')).toEqual(emptyDoc())
    expect(emptyDoc().content).toHaveLength(1)
  })

  it('recognises blank markdown', () => {
    expect(isBlankMarkdown('')).toBe(true)
    expect(isBlankMarkdown(null)).toBe(true)
    expect(isBlankMarkdown('  ')).toBe(true)
    expect(isBlankMarkdown('x')).toBe(false)
  })
})
