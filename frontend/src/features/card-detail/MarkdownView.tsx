import { Link } from '@tanstack/react-router'
import { Fragment, type ReactNode } from 'react'

import { cx } from '@/lib/cx'

import { splitCardKeys } from './autolink'
import type { Block, Doc, Inline, Mark } from './markdown'
import { parseMarkdown } from './markdown'
import styles from './MarkdownView.module.css'

/**
 * Renders stored markdown at read time — the sanitisation boundary.
 *
 * # Why this never touches `innerHTML`
 *
 * Descriptions and comments are other people's input, rendered into everyone else's browser.
 * The safe way to render untrusted markup is to **not build markup at all**: this walks the
 * parsed document and emits a fixed set of React elements, so text is escaped by React and
 * the only nodes that can ever exist are the ones this file names. There is no
 * `dangerouslySetInnerHTML`, so there is no sink for an injected `<script>` or `onerror` —
 * the class of bug is structurally absent rather than filtered. Link hrefs are the one place
 * an attacker controls an attribute, so they are validated to http/https/mailto and anything
 * else is rendered as inert text ([`safeHref`]).
 *
 * Card keys (`ATLAS-123`) are linked here, at render time, over the text runs — never in the
 * stored source (see `autolink.ts` and `markdown.ts`).
 */
export interface MarkdownViewProps {
  /** The markdown source. */
  source: string
  className?: string | undefined
}

export function MarkdownView({ source, className }: MarkdownViewProps) {
  const doc = parseMarkdown(source)
  return <div className={cx(styles.prose, className)}>{renderBlocks(doc.content)}</div>
}

function renderBlocks(blocks: Doc['content']): ReactNode {
  return blocks.map((block, index) => <Fragment key={index}>{renderBlock(block)}</Fragment>)
}

const HEADING_TAGS = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6'] as const

function renderBlock(block: Block): ReactNode {
  switch (block.type) {
    case 'heading': {
      const level = clampLevel(block.attrs?.level)
      const Tag = HEADING_TAGS[level - 1] ?? 'h3'
      return <Tag>{renderInline(inlineOf(block))}</Tag>
    }
    case 'paragraph':
      return <p>{renderInline(inlineOf(block))}</p>
    case 'blockquote':
      return <blockquote>{renderBlocks(childBlocks(block))}</blockquote>
    case 'codeBlock':
      return (
        <pre className={styles.codeBlock}>
          <code>{inlineOf(block).map((n) => (n.type === 'text' ? n.text : '')).join('')}</code>
        </pre>
      )
    case 'horizontalRule':
      return <hr className={styles.rule} />
    case 'bulletList':
      return <ul>{childBlocks(block).map((item, i) => renderListItem(item, i))}</ul>
    case 'orderedList': {
      const start = Number.isFinite(block.attrs?.start) ? Number(block.attrs!.start) : 1
      return (
        <ol start={start}>{childBlocks(block).map((item, i) => renderListItem(item, i))}</ol>
      )
    }
    case 'taskList':
      return (
        <ul className={styles.taskList}>
          {childBlocks(block).map((item, i) => (
            <li key={i} className={styles.taskItem}>
              <input
                type="checkbox"
                checked={Boolean(item.attrs?.checked)}
                readOnly
                aria-label={itemPlainText(item)}
                className={styles.taskCheckbox}
              />
              <span className={item.attrs?.checked ? styles.taskDone : undefined}>
                {renderItemInline(item)}
              </span>
            </li>
          ))}
        </ul>
      )
    default:
      return null
  }
}

function renderListItem(item: Block, key: number): ReactNode {
  return <li key={key}>{renderItemInline(item)}</li>
}

/** A list item holds a paragraph; render its inline directly to avoid a nested `<p>`. */
function renderItemInline(item: Block): ReactNode {
  const paragraph = childBlocks(item)[0]
  if (paragraph && (paragraph.type === 'paragraph' || paragraph.type === 'heading')) {
    return renderInline(inlineOf(paragraph))
  }
  return renderInline(inlineOf(item))
}

function renderInline(nodes: Inline[]): ReactNode {
  return nodes.map((node, index) => <Fragment key={index}>{renderInlineNode(node)}</Fragment>)
}

function renderInlineNode(node: Inline): ReactNode {
  if (node.type === 'hardBreak') return <br />
  if (node.type === 'mention') {
    return <span className={styles.mention}>@{node.attrs?.label ?? node.attrs?.id ?? ''}</span>
  }
  if (node.type !== 'text') return null

  // Card-key autolinking runs over the raw text of each run, before marks wrap it.
  let content: ReactNode = renderTextWithCardKeys(node.text)
  for (const mark of node.marks ?? []) {
    content = applyMark(mark, content)
  }
  return content
}

function applyMark(mark: Mark, content: ReactNode): ReactNode {
  switch (mark.type) {
    case 'bold':
      return <strong>{content}</strong>
    case 'italic':
      return <em>{content}</em>
    case 'strike':
      return <s>{content}</s>
    case 'code':
      return <code className={styles.code}>{content}</code>
    case 'link': {
      const href = safeHref(mark.attrs.href)
      if (href === null) return content
      const external = /^https?:/i.test(href)
      return (
        <a
          href={href}
          className={styles.link}
          {...(external ? { target: '_blank', rel: 'noopener noreferrer' } : {})}
        >
          {content}
        </a>
      )
    }
    default:
      return content
  }
}

/** Splits a text run on card keys and links each match. */
function renderTextWithCardKeys(text: string): ReactNode {
  const segments = splitCardKeys(text)
  if (segments.length === 1 && segments[0]?.kind === 'text') return text
  return segments.map((segment, index) =>
    segment.kind === 'card-key' ? (
      <Link key={index} to="/cards/$key" params={{ key: segment.key }} className={styles.cardKey}>
        {segment.text}
      </Link>
    ) : (
      <Fragment key={index}>{segment.text}</Fragment>
    ),
  )
}

/**
 * A link href, or `null` if it is not one Atlas will render as a link.
 *
 * Allowlist, not blocklist: only `http:`, `https:`, `mailto:` and site-relative paths pass.
 * A `javascript:` or `data:` URL — the classic markdown-link XSS — falls through to `null`
 * and the label renders as inert text.
 */
function safeHref(href: string): string | null {
  const trimmed = href.trim()
  if (trimmed.startsWith('/') || trimmed.startsWith('#')) return trimmed
  if (/^(https?|mailto):/i.test(trimmed)) return trimmed
  return null
}

function inlineOf(node: Block): Inline[] {
  return Array.isArray(node.content) ? (node.content as Inline[]).filter(isInline) : []
}

function childBlocks(node: Block): Block[] {
  return Array.isArray(node.content) ? (node.content as Block[]).filter((c) => !isInline(c)) : []
}

function isInline(node: Block | Inline): node is Inline {
  return node.type === 'text' || node.type === 'mention' || node.type === 'hardBreak'
}

function itemPlainText(item: Block): string {
  return inlineOf(childBlocks(item)[0] ?? item)
    .map((n) => (n.type === 'text' ? n.text : ''))
    .join('')
}

function clampLevel(value: unknown): number {
  const n = typeof value === 'number' ? value : 1
  return Math.min(6, Math.max(1, Math.trunc(n)))
}
