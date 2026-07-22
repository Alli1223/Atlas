/**
 * Markdown ⇄ ProseMirror-document conversion.
 *
 * # Why this exists at all
 *
 * The backend stores comment and description bodies as **markdown source** and never as
 * rendered HTML — a stored-HTML field is a stored-XSS hole with a scheduler attached (see
 * `crate::domain::comment::Comment`). TipTap, though, is a ProseMirror editor: it speaks a
 * document JSON, not markdown. So the editor's load path is `markdown → doc` and its save
 * path is `doc → markdown`, and both live here as pure functions.
 *
 * # One representation, not two
 *
 * The document shape is TipTap's own `JSONContent` — the exact thing `editor.getJSON()`
 * returns and `useEditor({ content })` accepts. Parsing straight to it (rather than to a
 * private AST that then needs a third converter into TipTap's schema) means the round-trip
 * test exercises the *actual* editor load/save path, and the read-time renderer
 * (`MarkdownView`) walks the same tree the editor holds.
 *
 * # The supported subset
 *
 * bold, italic, strike, inline code, links, @mentions · headings, paragraphs, bullet /
 * ordered / task lists (checklists), blockquotes, fenced code blocks, horizontal rules.
 * This is the ADS/Jira comment surface, not all of CommonMark — nested lists and reference
 * links are deliberately out, because every construct here has to survive the round trip and
 * an editor that silently drops what it cannot re-serialise is worse than one that never
 * accepted it.
 *
 * Card-key autolinking (`ATLAS-123` → a link) is **not** done here on purpose — it is a
 * render-time concern (`MarkdownView`), so the stored source stays `ATLAS-123` and the
 * round trip is stable.
 */

// ---------------------------------------------------------------------------
// The document shape — a structural subset of TipTap's JSONContent.
// ---------------------------------------------------------------------------

/** An inline mark. `link` carries an href; the rest are bare. */
export type Mark =
  | { type: 'bold' }
  | { type: 'italic' }
  | { type: 'strike' }
  | { type: 'code' }
  | { type: 'link'; attrs: { href: string } }

/** An inline node: a text run, a mention chip, or a hard line break. */
export type Inline =
  | { type: 'text'; text: string; marks?: Mark[] }
  | { type: 'mention'; attrs: { id: string; label: string } }
  | { type: 'hardBreak' }

/** A block node. Lists nest block content one level (a list item holds paragraphs). */
export interface Block {
  type:
    | 'paragraph'
    | 'heading'
    | 'bulletList'
    | 'orderedList'
    | 'listItem'
    | 'taskList'
    | 'taskItem'
    | 'blockquote'
    | 'codeBlock'
    | 'horizontalRule'
  attrs?: Record<string, unknown>
  content?: (Block | Inline)[]
}

/** A whole document. What TipTap holds and what the read view walks. */
export interface Doc {
  type: 'doc'
  content: Block[]
}

/** An empty document — a single empty paragraph, which is TipTap's own empty state. */
export function emptyDoc(): Doc {
  return { type: 'doc', content: [{ type: 'paragraph' }] }
}

// ---------------------------------------------------------------------------
// Parse: markdown → Doc
// ---------------------------------------------------------------------------

const HEADING = /^(#{1,6})\s+(.*)$/
const FENCE = /^```(\w*)\s*$/
const HR = /^(?:---|\*\*\*|___)\s*$/
const BLOCKQUOTE = /^>\s?(.*)$/
const TASK_ITEM = /^[-*+]\s+\[([ xX])\]\s+(.*)$/
const BULLET_ITEM = /^[-*+]\s+(.*)$/
const ORDERED_ITEM = /^(\d+)[.)]\s+(.*)$/

/**
 * Parses markdown source into a document.
 *
 * Block structure is recognised line by line; inline structure ([`parseInline`]) is a
 * separate pass over each block's text. An empty or whitespace-only source yields
 * [`emptyDoc`] rather than an empty content array, because ProseMirror's schema requires a
 * doc to hold at least one block and TipTap crashes on an empty one.
 */
export function parseMarkdown(source: string): Doc {
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  // `noUncheckedIndexedAccess` makes `lines[i]` `string | undefined`; within `i < length` it
  // is always a string, and blank lines are `''`, so `?? ''` maps the impossible undefined to
  // the same empty string a blank line already is.
  const at = (n: number): string => lines[n] ?? ''
  const blocks: Block[] = []
  let i = 0

  while (i < lines.length) {
    const line = at(i)

    if (line.trim() === '') {
      i += 1
      continue
    }

    // Fenced code block — content is literal, so no inline pass.
    const fence = FENCE.exec(line)
    if (fence) {
      const language = fence[1] ?? ''
      const body: string[] = []
      i += 1
      while (i < lines.length && !/^```\s*$/.test(at(i))) {
        body.push(at(i))
        i += 1
      }
      i += 1 // consume the closing fence (or run off the end)
      blocks.push({
        type: 'codeBlock',
        attrs: { language: language === '' ? null : language },
        content: body.length > 0 ? [{ type: 'text', text: body.join('\n') }] : [],
      })
      continue
    }

    const heading = HEADING.exec(line)
    if (heading) {
      blocks.push({
        type: 'heading',
        attrs: { level: (heading[1] ?? '#').length },
        content: parseInline(heading[2] ?? ''),
      })
      i += 1
      continue
    }

    if (HR.test(line)) {
      blocks.push({ type: 'horizontalRule' })
      i += 1
      continue
    }

    if (BLOCKQUOTE.test(line)) {
      const quoted: string[] = []
      while (i < lines.length && BLOCKQUOTE.test(at(i))) {
        quoted.push(BLOCKQUOTE.exec(at(i))?.[1] ?? '')
        i += 1
      }
      // Recurse: a blockquote holds blocks, so its inner lines parse as their own document.
      blocks.push({ type: 'blockquote', content: parseMarkdown(quoted.join('\n')).content })
      continue
    }

    // Task list — checked before the plain bullet, since `- [ ]` also matches a bullet.
    if (TASK_ITEM.test(line)) {
      const items: Block[] = []
      while (i < lines.length && TASK_ITEM.test(at(i))) {
        const m = TASK_ITEM.exec(at(i))
        items.push({
          type: 'taskItem',
          attrs: { checked: (m?.[1] ?? '').toLowerCase() === 'x' },
          content: [{ type: 'paragraph', content: parseInline(m?.[2] ?? '') }],
        })
        i += 1
      }
      blocks.push({ type: 'taskList', content: items })
      continue
    }

    if (BULLET_ITEM.test(line) && !TASK_ITEM.test(line)) {
      const items: Block[] = []
      while (i < lines.length && BULLET_ITEM.test(at(i)) && !TASK_ITEM.test(at(i))) {
        const m = BULLET_ITEM.exec(at(i))
        items.push({
          type: 'listItem',
          content: [{ type: 'paragraph', content: parseInline(m?.[1] ?? '') }],
        })
        i += 1
      }
      blocks.push({ type: 'bulletList', content: items })
      continue
    }

    const ordered = ORDERED_ITEM.exec(line)
    if (ordered) {
      const start = Number.parseInt(ordered[1] ?? '1', 10)
      const items: Block[] = []
      while (i < lines.length && ORDERED_ITEM.test(at(i))) {
        const m = ORDERED_ITEM.exec(at(i))
        items.push({
          type: 'listItem',
          content: [{ type: 'paragraph', content: parseInline(m?.[2] ?? '') }],
        })
        i += 1
      }
      blocks.push({
        type: 'orderedList',
        attrs: { start: Number.isNaN(start) ? 1 : start },
        content: items,
      })
      continue
    }

    // Paragraph — consecutive non-blank, non-special lines, joined by hard breaks so a
    // soft-wrapped paragraph survives the round trip as written.
    const para: string[] = []
    while (i < lines.length && at(i).trim() !== '' && !isBlockStart(at(i))) {
      para.push(at(i))
      i += 1
    }
    blocks.push({ type: 'paragraph', content: parseInlineLines(para) })
  }

  return blocks.length > 0 ? { type: 'doc', content: blocks } : emptyDoc()
}

/** Whether a line opens a non-paragraph block, so paragraph accumulation must stop. */
function isBlockStart(line: string): boolean {
  return (
    HEADING.test(line) ||
    FENCE.test(line) ||
    HR.test(line) ||
    BLOCKQUOTE.test(line) ||
    BULLET_ITEM.test(line) ||
    ORDERED_ITEM.test(line)
  )
}

/** Parses several paragraph lines into inline nodes separated by hard breaks. */
function parseInlineLines(lines: string[]): Inline[] {
  const out: Inline[] = []
  lines.forEach((line, index) => {
    if (index > 0) out.push({ type: 'hardBreak' })
    out.push(...parseInline(line))
  })
  return out
}

// The inline patterns, tried in order at each cursor position. Order matters: `**` before
// `*`, code before everything (its content is literal).
const INLINE = {
  code: /^`([^`]+)`/,
  bold: /^\*\*([^*]+?)\*\*/,
  strike: /^~~([^~]+?)~~/,
  emphasisStar: /^\*([^*]+?)\*/,
  emphasisUnderscore: /^_([^_]+?)_/,
  link: /^\[([^\]]*)\]\(([^)\s]+)\)/,
  mention: /^@([A-Za-z0-9._-]+)/,
}

/**
 * Parses one line of inline markdown into text/mention nodes.
 *
 * A hand-written scan rather than a regex-replace chain: emphasis can wrap other emphasis
 * (`**bold _and italic_**`), which a flat replace cannot express, so bold/italic/strike/link
 * recurse into their own content. Inline code does not recurse — its body is literal, which
 * is the whole point of code.
 */
export function parseInline(text: string): Inline[] {
  const nodes: Inline[] = []
  let buffer = ''
  let i = 0

  const flush = () => {
    if (buffer !== '') {
      nodes.push({ type: 'text', text: buffer })
      buffer = ''
    }
  }

  while (i < text.length) {
    const rest = text.slice(i)

    const code = INLINE.code.exec(rest)
    if (code) {
      flush()
      nodes.push({ type: 'text', text: code[1] ?? '', marks: [{ type: 'code' }] })
      i += code[0].length
      continue
    }

    const bold = INLINE.bold.exec(rest)
    if (bold) {
      flush()
      nodes.push(...withMark(parseInline(bold[1] ?? ''), { type: 'bold' }))
      i += bold[0].length
      continue
    }

    const strike = INLINE.strike.exec(rest)
    if (strike) {
      flush()
      nodes.push(...withMark(parseInline(strike[1] ?? ''), { type: 'strike' }))
      i += strike[0].length
      continue
    }

    const star = INLINE.emphasisStar.exec(rest)
    if (star) {
      flush()
      nodes.push(...withMark(parseInline(star[1] ?? ''), { type: 'italic' }))
      i += star[0].length
      continue
    }

    // `_` only opens emphasis at a word boundary, so `snake_case_name` stays literal.
    const prev = i === 0 ? '' : (text[i - 1] ?? '')
    if (rest.startsWith('_') && !/[A-Za-z0-9]/.test(prev)) {
      const underscore = INLINE.emphasisUnderscore.exec(rest)
      if (underscore) {
        flush()
        nodes.push(...withMark(parseInline(underscore[1] ?? ''), { type: 'italic' }))
        i += underscore[0].length
        continue
      }
    }

    const link = INLINE.link.exec(rest)
    if (link) {
      flush()
      const href = link[2] ?? ''
      const label = link[1] === '' || link[1] == null ? href : link[1]
      nodes.push(...withMark(parseInline(label), { type: 'link', attrs: { href } }))
      i += link[0].length
      continue
    }

    // A mention only at a boundary, so an email address is not shredded into one.
    if (rest.startsWith('@') && !/[A-Za-z0-9]/.test(prev)) {
      const mention = INLINE.mention.exec(rest)
      if (mention) {
        flush()
        const label = mention[1] ?? ''
        nodes.push({ type: 'mention', attrs: { id: label, label } })
        i += mention[0].length
        continue
      }
    }

    buffer += text[i] ?? ''
    i += 1
  }

  flush()
  return nodes
}

/** Adds a mark to every text node in a run, preserving marks already there. */
function withMark(nodes: Inline[], mark: Mark): Inline[] {
  return nodes.map((node) => {
    if (node.type !== 'text') return node
    return { ...node, marks: [...(node.marks ?? []), mark] }
  })
}

// ---------------------------------------------------------------------------
// Serialize: Doc → markdown
// ---------------------------------------------------------------------------

/**
 * Serialises a document (TipTap's `getJSON()` output) back to markdown source.
 *
 * The canonical spellings — `**` for bold, `*` for italic, `- ` for bullets, `` ` `` fences —
 * are fixed so that `serializeMarkdown(parseMarkdown(canonical))` is exactly `canonical`.
 * Robust to the loose JSON TipTap actually emits: absent `content`, absent `attrs`, unknown
 * node types are all handled rather than assumed away.
 */
export function serializeMarkdown(doc: Doc | { type: string; content?: unknown }): string {
  const content: unknown = doc.content
  if (!Array.isArray(content)) return ''
  return serializeBlocks(content as Block[]).replace(/\n+$/, '') + '\n'
}

function serializeBlocks(blocks: Block[]): string {
  return blocks.map(serializeBlock).join('\n')
}

function serializeBlock(block: Block): string {
  switch (block.type) {
    case 'heading': {
      const level = clampLevel(block.attrs?.level)
      return `${'#'.repeat(level)} ${serializeInline(inlineOf(block))}\n`
    }
    case 'paragraph':
      return `${serializeInline(inlineOf(block))}\n`
    case 'blockquote':
      return `${prefixLines(serializeBlocks(blockChildren(block)), '> ')}\n`
    case 'codeBlock': {
      const lang = block.attrs?.language
      const language = typeof lang === 'string' ? lang : ''
      const text = inlineOf(block)
        .map((n) => (n.type === 'text' ? n.text : ''))
        .join('')
      return `\`\`\`${language}\n${text}\n\`\`\`\n`
    }
    case 'horizontalRule':
      return '---\n'
    case 'bulletList':
      return (
        blockChildren(block)
          .map((item) => `- ${itemText(item)}`)
          .join('\n') + '\n'
      )
    case 'orderedList': {
      const start = Number.isFinite(block.attrs?.start) ? Number(block.attrs!.start) : 1
      return (
        blockChildren(block)
          .map((item, index) => `${start + index}. ${itemText(item)}`)
          .join('\n') + '\n'
      )
    }
    case 'taskList':
      return (
        blockChildren(block)
          .map((item) => `- [${item.attrs?.checked ? 'x' : ' '}] ${itemText(item)}`)
          .join('\n') + '\n'
      )
    default:
      return ''
  }
}

/** A list item's single-paragraph text. Items hold a paragraph; we render its inline. */
function itemText(item: Block): string {
  const first = blockChildren(item)[0]
  if (first && (first.type === 'paragraph' || first.type === 'heading')) {
    return serializeInline(inlineOf(first))
  }
  // A bare-text item (some TipTap configs), or an empty one.
  return serializeInline(inlineOf(item))
}

function serializeInline(nodes: Inline[]): string {
  return nodes.map(serializeInlineNode).join('')
}

function serializeInlineNode(node: Inline): string {
  if (node.type === 'hardBreak') return '\n'
  if (node.type === 'mention') return `@${node.attrs?.label ?? node.attrs?.id ?? ''}`
  if (node.type !== 'text') return ''

  let text = node.text
  const marks = node.marks ?? []
  // Code wins and is literal: a code span never also carries emphasis in this subset.
  if (marks.some((m) => m.type === 'code')) return `\`${text}\``

  // Innermost → outermost, so the closing order mirrors the opening order.
  if (marks.some((m) => m.type === 'italic')) text = `*${text}*`
  if (marks.some((m) => m.type === 'strike')) text = `~~${text}~~`
  if (marks.some((m) => m.type === 'bold')) text = `**${text}**`
  const link = marks.find((m): m is Extract<Mark, { type: 'link' }> => m.type === 'link')
  if (link) text = `[${text}](${link.attrs.href})`
  return text
}

// --- small shared helpers over the loose JSON --------------------------------

function inlineOf(node: Block): Inline[] {
  return Array.isArray(node.content) ? (node.content as Inline[]).filter(isInline) : []
}

function blockChildren(node: Block): Block[] {
  return Array.isArray(node.content) ? (node.content as Block[]).filter((c) => !isInline(c)) : []
}

function isInline(node: Block | Inline): node is Inline {
  return node.type === 'text' || node.type === 'mention' || node.type === 'hardBreak'
}

function clampLevel(value: unknown): number {
  const n = typeof value === 'number' ? value : 1
  return Math.min(6, Math.max(1, Math.trunc(n)))
}

function prefixLines(text: string, prefix: string): string {
  return text
    .replace(/\n+$/, '')
    .split('\n')
    .map((line) => `${prefix}${line}`)
    .join('\n')
}

/** Whether a markdown string has any content once trimmed. */
export function isBlankMarkdown(source: string | null | undefined): boolean {
  return source == null || source.trim() === ''
}
