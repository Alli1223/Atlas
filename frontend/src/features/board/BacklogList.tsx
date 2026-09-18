import { autoScrollForElements } from '@atlaskit/pragmatic-drag-and-drop-auto-scroll/element'
import { dropTargetForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import { useEffect, useRef, useState } from 'react'

import { cx } from '@/lib/cx'

import type { BoardCard as BoardCardData } from './api'
import type { BacklogListKey } from './queries'
import styles from './BacklogList.module.css'
import { BoardCard, type CardReferences } from './BoardCard'

export interface BacklogListProps {
  /** `null` for the plain backlog, a cycle id otherwise — rides the drop-target data. */
  listKey: BacklogListKey
  title: string
  /** A short status line under the title — a cycle's dates/goal, or nothing for the backlog. */
  subtitle?: string
  cards: BoardCardData[]
  references: CardReferences
  onOpen: (cardKey: string) => void
  onOpenBoard: (cardKey: string) => void
}

/**
 * One backlog section: a header and a drop-targetable, vertically-stacked list of cards.
 *
 * Unlike a board column, there is no rank to resolve here — a card dropped anywhere in the
 * list just joins (or leaves) this list's cycle. [`BoardCard`]'s own per-card drop target
 * still renders its edge indicator while dragging over another card (it is a shared
 * component), but nothing here reads that edge; [`BacklogView`]'s monitor only cares which
 * list the pointer landed in.
 */
export function BacklogList({
  listKey,
  title,
  subtitle,
  cards,
  references,
  onOpen,
  onOpenBoard,
}: BacklogListProps) {
  const listRef = useRef<HTMLDivElement>(null)
  const [isDraggedOver, setIsDraggedOver] = useState(false)

  useEffect(() => {
    const element = listRef.current
    if (!element) return

    const data = { type: 'backlog-list', listKey }

    return dropTargetForElements({
      element,
      canDrop: ({ source }) => source.data.type === 'card',
      getData: () => data,
      onDragEnter: () => setIsDraggedOver(true),
      onDragLeave: () => setIsDraggedOver(false),
      onDrop: () => setIsDraggedOver(false),
    })
  }, [listKey])

  useEffect(() => {
    const element = listRef.current
    if (!element) return
    return autoScrollForElements({
      element,
      canScroll: ({ source }) => source.data.type === 'card',
    })
  }, [])

  return (
    <section className={styles.section} aria-label={title}>
      <header className={styles.header}>
        <span className={styles.title}>{title}</span>
        {subtitle && <span className={styles.subtitle}>{subtitle}</span>}
        <span className={styles.count}>{cards.length}</span>
      </header>

      <div
        ref={listRef}
        className={cx(styles.list, isDraggedOver && styles.listOver)}
      >
        {cards.map((card) => (
          <BoardCard
            key={card.id}
            card={card}
            laneKey={listKey ?? ''}
            references={references}
            onOpen={onOpen}
            {...(card.childRollup ? { onOpenBoard } : {})}
          />
        ))}
        {cards.length === 0 && (
          <div className={styles.empty} aria-hidden="true">
            Drop a card here
          </div>
        )}
      </div>
    </section>
  )
}
