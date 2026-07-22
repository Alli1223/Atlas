import { LayoutGrid } from 'lucide-react'

import { cx } from '@/lib/cx'
import { ICON_SMALL } from '@/lib/icon'

import type { ChildRollup } from './api'
import styles from './CardMiniMap.module.css'

export interface CardMiniMapProps {
  rollup: ChildRollup
}

/** A status category, in board (left-to-right) order. `key` also indexes [`BlockCounts`]. */
interface Category {
  key: keyof BlockCounts
  /** The colour class, shared by the mini-board blocks and the progress bar segment. */
  className: string | undefined
  /** Spoken label for the accessible summary. */
  label: string
}

const CATEGORIES: readonly Category[] = [
  { key: 'todo', className: styles.todo, label: 'to do' },
  { key: 'inProgress', className: styles.inProgress, label: 'in progress' },
  { key: 'done', className: styles.done, label: 'done' },
]

/** The tallest mini-column draws this many blocks; the others scale to it. */
const MAX_BLOCKS = 5

/** How many blocks each mini-column draws, per category. */
export interface BlockCounts {
  todo: number
  inProgress: number
  done: number
}

/**
 * How many little blocks each mini-column draws.
 *
 * The busiest category gets [`MAX_BLOCKS`]; the rest scale proportionally, with a floor of
 * one block for any non-zero count so a lone card is never invisible. The blocks convey the
 * *shape* of the child board — "mostly to-do" vs "mostly done" — at a glance; the exact
 * figures live in the progress label beneath. Pure and exported so it is unit-testable.
 */
export function miniBoardBlocks(rollup: ChildRollup): BlockCounts {
  const peak = Math.max(rollup.todo, rollup.inProgress, rollup.done, 1)
  const scale = (n: number) => (n <= 0 ? 0 : Math.max(1, Math.round((n / peak) * MAX_BLOCKS)))
  return { todo: scale(rollup.todo), inProgress: scale(rollup.inProgress), done: scale(rollup.done) }
}

/**
 * The affordance that a card *contains a board*: a miniature of the child board — three
 * status-category columns of tiny blocks — over a `n/total done` progress bar.
 *
 * This is the recursive-board signal. A card with children is itself a board, and this is
 * how that is visible at a glance without opening it: the three columns show the child
 * distribution (To Do grey, In Progress blue, Done green), so a card whose children are
 * mostly done reads as a tall green column, and a fresh one as a tall grey column. The
 * backend sends the rollup on every parent in one query (never N+1); a leaf card has none,
 * so this simply is not rendered.
 */
export function CardMiniMap({ rollup }: CardMiniMapProps) {
  const blocks = miniBoardBlocks(rollup)
  const total = Math.max(rollup.total, 1)

  // The progress bar reads done → in-progress → to-do so "done" grows from the left as work
  // completes. Empty categories are dropped so a single non-zero one still fills the bar.
  const segments = [
    { key: 'done', value: rollup.done, className: styles.done },
    { key: 'inProgress', value: rollup.inProgress, className: styles.inProgress },
    { key: 'todo', value: rollup.todo, className: styles.todo },
  ].filter((segment) => segment.value > 0)

  return (
    <div className={styles.miniMap}>
      <div
        className={styles.miniBoard}
        role="img"
        aria-label={`Child board: ${rollup.todo} to do, ${rollup.inProgress} in progress, ${rollup.done} done`}
      >
        {CATEGORIES.map((category) => {
          const count = blocks[category.key]
          return (
            <div
              key={category.key}
              className={styles.column}
              data-category={category.key}
              data-blocks={count}
            >
              {Array.from({ length: count }, (_, index) => (
                <span key={index} className={cx(styles.block, category.className)} />
              ))}
            </div>
          )
        })}
      </div>

      <div className={styles.progress}>
        <span className={styles.icon} aria-hidden="true">
          <LayoutGrid {...ICON_SMALL} />
        </span>
        <div
          className={styles.bar}
          role="img"
          aria-label={`${rollup.done} of ${rollup.total} child cards done`}
        >
          {segments.map((segment) => (
            <span
              key={segment.key}
              className={segment.className}
              style={{ flexGrow: segment.value / total }}
            />
          ))}
        </div>
        <span className={styles.label}>
          {rollup.done}/{rollup.total} done
        </span>
      </div>
    </div>
  )
}
