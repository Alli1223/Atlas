import { Tag as TagChip } from '@/components/ui'
import { cx } from '@/lib/cx'

import type { Tag } from './api'
import styles from './TagList.module.css'

export interface TagListProps {
  tags: readonly Tag[]
  /** Renders a remove affordance on each chip. Omit for a read-only list. */
  onRemove?: (tag: Tag) => void
  /** Builds a filter link for each chip — "show me every card with this tag". */
  hrefForTag?: (tag: Tag) => string
  /** Shown when there are no tags. Omit to render nothing at all. */
  emptyMessage?: string
  /** Accessible name for the list. @default 'Tags' */
  label?: string
  className?: string | undefined
}

/**
 * A card's tags, as chips.
 *
 * # Why this is a `ul` and not a row of spans
 *
 * A screen reader announcing "list, 4 items: bug, hotfix, blocked, needs-review" conveys
 * what a sighted user gets from four coloured chips in a row. Four unrelated spans convey
 * a sentence: "bug hotfix blocked needs-review".
 *
 * # Why the colour is never the only signal
 *
 * Every chip carries its name as text. The colour groups related tags at a glance, but it
 * is decoration — WCAG 1.4.1, and also just true of anyone who has not memorised what
 * teal means on this board.
 */
export function TagList({
  tags,
  onRemove,
  hrefForTag,
  emptyMessage,
  label = 'Tags',
  className,
}: TagListProps) {
  if (tags.length === 0) {
    return emptyMessage !== undefined ? <p className={styles.empty}>{emptyMessage}</p> : null
  }

  return (
    <ul className={cx(styles.list, className)} aria-label={label}>
      {tags.map((tag) => (
        <li key={tag.id} className={styles.item}>
          <TagChip
            // `colour` is nullable server-side and means "no colour chosen", which the
            // primitive spells `standard`.
            color={tag.colour ?? 'standard'}
            {...(hrefForTag !== undefined && { href: hrefForTag(tag) })}
            {...(onRemove !== undefined && {
              onRemove: () => onRemove(tag),
              removeButtonLabel: `Remove tag ${tag.name}`,
            })}
          >
            {tag.name}
          </TagChip>
        </li>
      ))}
    </ul>
  )
}
