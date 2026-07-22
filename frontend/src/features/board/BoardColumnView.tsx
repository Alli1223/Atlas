import { autoScrollForElements } from '@atlaskit/pragmatic-drag-and-drop-auto-scroll/element'
import { combine } from '@atlaskit/pragmatic-drag-and-drop/combine'
import { dropTargetForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import { useEffect, useRef, useState } from 'react'

import { cx } from '@/lib/cx'

import type { BoardColumn } from './api'
import { BoardCard, type CardReferences } from './BoardCard'
import styles from './BoardColumnView.module.css'

export interface BoardColumnViewProps {
  column: BoardColumn
  /** The lane this column instance belongs to — `''` for the flat board. */
  laneKey: string
  references: CardReferences
  onOpen: (cardKey: string) => void
  onOpenBoard: (cardKey: string) => void
  /** The WIP limit for this status, if the active board sets one. */
  wipLimit?: number
}

/**
 * One board column: a sunken well with a status header and a scrollable, drop-targetable
 * list of cards.
 *
 * The list element is both a `dropTargetForElements` (so a card can be dropped into an empty
 * column or below the last card) and an `autoScrollForElements` target — the auto-scroll is
 * the single hardest thing to get right by hand on a multi-column board, and using
 * Atlassian's own tuned implementation is most of what makes the drag *feel* like Jira.
 */
export function BoardColumnView({
  column,
  laneKey,
  references,
  onOpen,
  onOpenBoard,
  wipLimit,
}: BoardColumnViewProps) {
  const listRef = useRef<HTMLDivElement>(null)
  const [isDraggedOver, setIsDraggedOver] = useState(false)

  const statusId = column.status.id
  const count = column.cards.length
  const breached = wipLimit !== undefined && count > wipLimit

  useEffect(() => {
    const element = listRef.current
    if (!element) return

    const data = { type: 'column', statusId, laneKey }

    return combine(
      dropTargetForElements({
        element,
        canDrop: ({ source }) => source.data.type === 'card',
        getData: () => data,
        onDragEnter: () => setIsDraggedOver(true),
        onDragLeave: () => setIsDraggedOver(false),
        onDrop: () => setIsDraggedOver(false),
      }),
      autoScrollForElements({
        element,
        canScroll: ({ source }) => source.data.type === 'card',
      }),
    )
  }, [statusId, laneKey])

  return (
    <section className={styles.column} aria-label={column.status.name}>
      <header
        className={cx(styles.header, breached && styles.breached)}
        data-wip-breached={breached}
      >
        <span className={styles.name}>{column.status.name}</span>
        <span className={styles.count}>
          {wipLimit !== undefined ? `${count}/${wipLimit}` : count}
        </span>
      </header>

      <div
        ref={listRef}
        className={cx(styles.list, isDraggedOver && styles.listOver)}
        data-status-category={column.status.category}
      >
        {column.cards.map((card) => (
          <BoardCard
            key={card.id}
            card={card}
            laneKey={laneKey}
            references={references}
            onOpen={onOpen}
            {...(card.childRollup ? { onOpenBoard } : {})}
          />
        ))}
        {count === 0 && <div className={styles.empty} aria-hidden="true" />}
      </div>
    </section>
  )
}
