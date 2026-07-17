import { Check, Plus } from 'lucide-react'
import { useId, useRef, useState } from 'react'

import { Tag as TagChip } from '@/components/ui'
import { cx } from '@/lib/cx'
import { ICON_SMALL } from '@/lib/icon'

import type { Tag, TagUsage } from './api'
import { hyphenate, rankTags, tagNameErrorMessage, validateTagName } from './name'
import styles from './TagPicker.module.css'

export interface TagPickerProps {
  /** Every tag the project offers — its own and every global one, with usage counts. */
  options: readonly TagUsage[]
  /** The tags already on the card. Rendered as selected; picking one removes it. */
  selected: readonly Tag[]
  /** Called when an existing tag is picked. */
  onSelect: (tag: Tag) => void
  /** Called when a selected tag is picked again, or its chip is dismissed. */
  onDeselect: (tag: Tag) => void
  /** Called when the typed name matches nothing and the user asks to create it. */
  onCreate: (name: string) => void
  /** Disables create-on-the-fly, e.g. for a read-only filter picker. */
  canCreate?: boolean
  /** True while a create is in flight. */
  isCreating?: boolean
  /** @default 'Add a tag' */
  placeholder?: string
  /** @default 'Tags' */
  label?: string
  className?: string | undefined
}

/** How many options render before the list scrolls. Keeps a 15-tag preset list usable. */
const VISIBLE_OPTIONS = 8

/**
 * Autocomplete over a project's tags, with create-on-the-fly.
 *
 * # The interaction this is copying, and why
 *
 * Type to filter, arrow to move, Enter to pick, Enter on no match to create. That is
 * Jira's label picker, GitHub's, and Linear's, because it is the one interaction where a
 * user can add a label without ever learning that labels are a thing you configure. The
 * whole argument for Phase 4 being ⭐ rests on this staying a two-second interaction.
 *
 * # Why the ARIA is spelled out rather than reached for from a library
 *
 * This is the `combobox` + `listbox` pattern: the input owns `aria-expanded`,
 * `aria-controls` and `aria-activedescendant`, and the *options* are `option` elements
 * whose selected state is `aria-selected`. Focus never leaves the input — that is the
 * point of `aria-activedescendant`, and it is what lets someone keep typing to narrow the
 * list while an option is highlighted. A `div` with click handlers looks identical and is
 * unusable without a mouse.
 *
 * # What create-on-the-fly must not do
 *
 * Silently fix the name. `needs review` is offered back as `needs-review` for the user to
 * accept — the same rule the backend enforces, surfaced before the round trip rather than
 * as a 422 afterwards. See `name.ts`.
 */
export function TagPicker({
  options,
  selected,
  onSelect,
  onDeselect,
  onCreate,
  canCreate = true,
  isCreating = false,
  placeholder = 'Add a tag',
  label = 'Tags',
  className,
}: TagPickerProps) {
  const [query, setQuery] = useState('')
  const [isOpen, setIsOpen] = useState(false)
  const [activeIndex, setActiveIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  const listboxId = useId()
  const inputId = useId()
  const errorId = useId()

  // No useMemo anywhere in here, deliberately: the React Compiler (wired up in
  // vite.config.ts) memoizes these derivations itself, and a hand-written useMemo it
  // cannot prove equivalent makes it skip optimizing the *whole component* — trading
  // three cached values for every other value in the file. Nothing else in `src/` reaches
  // for useMemo either. The lists here are a project's tags: tens of items, not thousands.
  const selectedIds = new Set(selected.map((t) => t.id))

  const matches = rankTags(options, query).slice(0, 50)

  const trimmed = query.trim()
  const nameError = trimmed.length > 0 ? validateTagName(trimmed) : null

  // An exact (case-insensitive) hit means "pick that", never "create a duplicate" — the
  // server's names are COLLATE NOCASE and would answer a 409.
  const exactMatch = options.find((t) => t.name.toLowerCase() === trimmed.toLowerCase())

  // Offer creation for a *whitespace* error too: the suggestion is the fix, and refusing
  // outright would leave the user staring at a rule with no way forward.
  const suggestion = nameError === 'whitespace' ? hyphenate(trimmed) : trimmed
  const suggestionIsTaken =
    nameError === 'whitespace' &&
    options.some((t) => t.name.toLowerCase() === suggestion.toLowerCase())

  const canOfferCreate =
    canCreate &&
    trimmed.length > 0 &&
    exactMatch === undefined &&
    !suggestionIsTaken &&
    (nameError === null || nameError === 'whitespace')

  // The create row lives at the end of the list, so it has an index in the same space as
  // the options — which is what makes one arrow-key handler serve both.
  const rowCount = matches.length + (canOfferCreate ? 1 : 0)
  const createIndex = canOfferCreate ? matches.length : -1
  const activeRow = Math.min(activeIndex, Math.max(rowCount - 1, 0))

  const errorMessage =
    nameError !== null && nameError !== 'whitespace'
      ? tagNameErrorMessage(nameError, trimmed)
      : suggestionIsTaken
        ? `“${suggestion}” already exists — pick it from the list.`
        : null

  function open() {
    setIsOpen(true)
  }

  function close() {
    setIsOpen(false)
    setActiveIndex(0)
  }

  function commitCreate() {
    if (!canOfferCreate) return
    onCreate(suggestion)
    setQuery('')
    close()
  }

  function toggle(tag: Tag) {
    if (selectedIds.has(tag.id)) onDeselect(tag)
    else onSelect(tag)
    setQuery('')
    // Deliberately stays open: adding three tags in a row is the common case, and a
    // picker that closes after each one makes it three round trips through the button.
    setActiveIndex(0)
    inputRef.current?.focus()
  }

  function commitActiveRow() {
    if (rowCount === 0) return
    if (activeRow === createIndex) commitCreate()
    else {
      const tag = matches[activeRow]
      if (tag !== undefined) toggle(tag)
    }
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        open()
        // Wraps, because a list that stops dead at the bottom makes you reverse out of it.
        setActiveIndex((i) => (rowCount === 0 ? 0 : (i + 1) % rowCount))
        break
      case 'ArrowUp':
        event.preventDefault()
        open()
        setActiveIndex((i) => (rowCount === 0 ? 0 : (i - 1 + rowCount) % rowCount))
        break
      case 'Enter':
        // Only swallow Enter when it means something here. Otherwise it belongs to the
        // form this picker sits in, and stealing it would break submit-on-Enter.
        if (isOpen && rowCount > 0) {
          event.preventDefault()
          commitActiveRow()
        }
        break
      case 'Escape':
        if (isOpen) {
          event.preventDefault()
          close()
        }
        break
      case 'Backspace':
        // The convention every chip input shares: backspace on an empty field takes the
        // last chip off, so correcting a mis-click needs no mouse.
        if (query.length === 0 && selected.length > 0) {
          const last = selected[selected.length - 1]
          if (last !== undefined) onDeselect(last)
        }
        break
      default:
        break
    }
  }

  return (
    <div
      className={cx(styles.picker, className)}
      // A click anywhere outside closes the list. `onBlur` on a container with
      // `relatedTarget` rather than a document listener: it fires for keyboard tabbing out
      // as well, which a click listener would miss entirely.
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) close()
      }}
    >
      <label className={styles.label} htmlFor={inputId}>
        {label}
      </label>

      <div className={styles.control}>
        {selected.length > 0 && (
          <ul className={styles.chips} aria-label={`${label}, selected`}>
            {selected.map((tag) => (
              <li key={tag.id} className={styles.chip}>
                <TagChip
                  color={tag.colour ?? 'standard'}
                  onRemove={() => onDeselect(tag)}
                  removeButtonLabel={`Remove tag ${tag.name}`}
                >
                  {tag.name}
                </TagChip>
              </li>
            ))}
          </ul>
        )}

        <input
          ref={inputRef}
          id={inputId}
          type="text"
          role="combobox"
          className={styles.input}
          value={query}
          placeholder={placeholder}
          autoComplete="off"
          aria-expanded={isOpen}
          aria-controls={listboxId}
          aria-autocomplete="list"
          aria-activedescendant={
            isOpen && rowCount > 0 ? `${listboxId}-row-${activeRow}` : undefined
          }
          aria-describedby={errorMessage !== null ? errorId : undefined}
          aria-invalid={errorMessage !== null || undefined}
          onChange={(event) => {
            setQuery(event.target.value)
            setActiveIndex(0)
            open()
          }}
          onFocus={open}
          onKeyDown={handleKeyDown}
        />
      </div>

      {errorMessage !== null && (
        <p id={errorId} className={styles.error} role="alert">
          {errorMessage}
        </p>
      )}

      {isOpen && (
        <ul
          id={listboxId}
          role="listbox"
          aria-label={`${label}, suggestions`}
          className={styles.listbox}
          style={{ '--visible-options': VISIBLE_OPTIONS } as React.CSSProperties}
        >
          {matches.map((tag, index) => {
            const isSelected = selectedIds.has(tag.id)
            return (
              <li
                key={tag.id}
                id={`${listboxId}-row-${index}`}
                role="option"
                aria-selected={isSelected}
                className={cx(styles.option, index === activeRow && styles.active)}
                // onMouseDown, not onClick: mousedown fires before the input's blur, so
                // the option is still there to be clicked. With onClick the container's
                // onBlur closes the list first and the click lands on nothing — the
                // classic "the dropdown ignores my mouse" bug.
                onMouseDown={(event) => {
                  event.preventDefault()
                  toggle(tag)
                }}
                onMouseEnter={() => setActiveIndex(index)}
              >
                <span className={styles.check} aria-hidden="true">
                  {isSelected && <Check {...ICON_SMALL} />}
                </span>
                <TagChip color={tag.colour ?? 'standard'}>{tag.name}</TagChip>
                <span className={styles.count}>
                  {/* The number is why the picker is worth reading: it says which tags
                      this board actually uses, so the list sorts itself in the user's
                      head. `0` is honest and useful — a preset nobody has used yet. */}
                  {tag.usageCount}
                </span>
              </li>
            )
          })}

          {canOfferCreate && (
            <li
              id={`${listboxId}-row-${createIndex}`}
              role="option"
              aria-selected={false}
              className={cx(styles.option, styles.create, activeRow === createIndex && styles.active)}
              onMouseDown={(event) => {
                event.preventDefault()
                commitCreate()
              }}
              onMouseEnter={() => setActiveIndex(createIndex)}
            >
              <span className={styles.check} aria-hidden="true">
                <Plus {...ICON_SMALL} />
              </span>
              <span className={styles.createLabel}>
                {isCreating ? 'Creating…' : 'Create'} <strong>{suggestion}</strong>
                {/* The rule, shown at the moment it applies rather than as a hint nobody
                    reads before typing. */}
                {nameError === 'whitespace' && (
                  <span className={styles.hint}> — tag names cannot contain spaces</span>
                )}
              </span>
            </li>
          )}

          {rowCount === 0 && (
            <li className={styles.emptyRow} role="presentation">
              {trimmed.length > 0 ? 'No matching tags' : 'No tags yet'}
            </li>
          )}
        </ul>
      )}
    </div>
  )
}
