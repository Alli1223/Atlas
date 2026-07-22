import { ChevronRight } from 'lucide-react'
import { useState } from 'react'

import { Avatar } from '@/components/ui'
import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'

import type { BoardSwimlane } from './api'
import type { CardReferences } from './BoardCard'
import { BoardColumnView } from './BoardColumnView'
import styles from './SwimlaneGroup.module.css'

export interface SwimlaneGroupProps {
  lane: BoardSwimlane
  references: CardReferences
  wipLimits: Record<string, number>
  onOpen: (cardKey: string) => void
  onOpenBoard: (cardKey: string) => void
}

/** One swimlane: a collapsible labelled band holding the same columns as the flat board. */
export function SwimlaneGroup({
  lane,
  references,
  wipLimits,
  onOpen,
  onOpenBoard,
}: SwimlaneGroupProps) {
  const [collapsed, setCollapsed] = useState(false)
  const count = lane.columns.reduce((sum, column) => sum + column.cards.length, 0)

  // A named lane (assignee/parent) carries an id in `key`; the catch-all lane is `''`.
  const user = lane.key !== '' ? references.userById.get(lane.key) : undefined

  return (
    <section className={styles.lane} aria-label={lane.label}>
      <button
        type="button"
        className={styles.header}
        onClick={() => setCollapsed((value) => !value)}
        aria-expanded={!collapsed}
      >
        <span className={cx(styles.chevron, collapsed && styles.chevronCollapsed)}>
          <ChevronRight {...ICON} aria-hidden="true" />
        </span>
        {user && <Avatar name={user.displayName} size="small" />}
        <span className={styles.label}>{lane.label}</span>
        <span className={styles.count}>{count}</span>
      </button>

      {!collapsed && (
        <div className={styles.columns}>
          {lane.columns.map((column) => (
            <BoardColumnView
              key={column.status.id}
              column={column}
              laneKey={lane.key}
              references={references}
              onOpen={onOpen}
              onOpenBoard={onOpenBoard}
              {...(wipLimits[column.status.id] !== undefined
                ? { wipLimit: wipLimits[column.status.id] }
                : {})}
            />
          ))}
        </div>
      )}
    </section>
  )
}
