import { useEffect, useRef, useState } from 'react'

import { cx } from '@/lib/cx'

import styles from './CardDetail.module.css'

export interface InlineTextProps {
  /** The current value, from the (optimistically updated) card. */
  value: string
  /** Called with the trimmed new value when the edit commits. Not called if unchanged. */
  onCommit: (value: string) => void
  /** Renders the value as a heading vs. plain text. @default false */
  isHeading?: boolean
  /** Accessible label for the edit control. */
  label: string
  placeholder?: string
  /** Reject an empty commit (e.g. a summary must not be blank). @default false */
  required?: boolean
}

/**
 * Click-to-edit text — the summary field.
 *
 * The interaction is Jira's: the value looks like text until you click it, then it is an
 * input focused with the caret at the end. Enter or blur commits; Escape reverts. There is
 * no separate edit modal and no explicit Save button, which is the whole point of *inline*
 * edit — the save is [`usePatchCard`]'s optimistic write, so the new value is on screen
 * before the request resolves, and rolls back if it fails.
 *
 * A commit that did not change anything does not call `onCommit`, so clicking a field and
 * clicking away never writes a no-op history row.
 */
export function InlineText({
  value,
  onCommit,
  isHeading = false,
  label,
  placeholder,
  required = false,
}: InlineTextProps) {
  const [isEditing, setIsEditing] = useState(false)
  const [draft, setDraft] = useState(value)
  const inputRef = useRef<HTMLTextAreaElement>(null)

  // The draft is seeded at the click that opens editing (below), not in an effect — so the
  // effect here only drives the DOM (focus + caret + autosize), which is what an effect is
  // for. Syncing state from a prop inside an effect would cascade an extra render.
  useEffect(() => {
    if (!isEditing) return
    const el = inputRef.current
    if (el) {
      el.focus()
      el.setSelectionRange(el.value.length, el.value.length)
      autosize(el)
    }
  }, [isEditing])

  function open() {
    setDraft(value)
    setIsEditing(true)
  }

  function commit() {
    const next = draft.trim()
    setIsEditing(false)
    if (required && next === '') return
    if (next !== value) onCommit(next)
  }

  function cancel() {
    setDraft(value)
    setIsEditing(false)
  }

  const shown = value === '' ? '' : value
  if (!isEditing) {
    return (
      <button
        type="button"
        className={cx(styles.inlineValue, isHeading && styles.inlineHeading)}
        onClick={open}
        aria-label={`${label}: ${value === '' ? (placeholder ?? 'empty') : value}. Click to edit.`}
      >
        {shown === '' ? (
          <span className={styles.inlinePlaceholder}>{placeholder}</span>
        ) : (
          shown
        )}
      </button>
    )
  }

  return (
    <textarea
      ref={inputRef}
      className={cx(styles.inlineInput, isHeading && styles.inlineHeading)}
      value={draft}
      rows={1}
      aria-label={label}
      onChange={(event) => {
        setDraft(event.target.value)
        autosize(event.target)
      }}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === 'Enter' && !event.shiftKey) {
          event.preventDefault()
          commit()
        } else if (event.key === 'Escape') {
          event.preventDefault()
          cancel()
        }
      }}
    />
  )
}

/** Grows a textarea to fit its content, so the summary never scrolls internally. */
function autosize(el: HTMLTextAreaElement): void {
  el.style.height = 'auto'
  el.style.height = `${el.scrollHeight}px`
}
