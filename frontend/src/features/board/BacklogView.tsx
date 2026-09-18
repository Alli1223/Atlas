import { monitorForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import { useEffect, useRef } from 'react'

import type { BacklogData } from './api'
import { BacklogList } from './BacklogList'
import styles from './BacklogView.module.css'
import type { CardReferences } from './BoardCard'
import { type BacklogListKey, useMoveCardToCycle } from './queries'
import { resolveBacklogDrop } from './resolveBacklogDrop'

export interface BacklogViewProps {
  projectKey: string
  backlog: BacklogData
  references: CardReferences
  onOpen: (cardKey: string) => void
  onOpenBoard: (cardKey: string) => void
}

/**
 * The backlog canvas: the plain backlog plus one section per non-closed cycle, and the
 * single drag monitor that turns a drop into a cycle-membership change.
 *
 * Simpler than [`BoardView`]'s monitor by design — there is no rank to resolve, so a drop
 * only needs to know *which list* it landed in, never *where within it*. That resolution is
 * [`resolveBacklogDrop`], pure and unit-tested; this monitor is thin, untestable-in-jsdom glue
 * around it, exactly the split `resolveDrop`/`BoardView` use for the main board.
 */
export function BacklogView({
  projectKey,
  backlog,
  references,
  onOpen,
  onOpenBoard,
}: BacklogViewProps) {
  const move = useMoveCardToCycle(projectKey)

  const backlogRef = useRef(backlog)
  const moveRef = useRef(move)
  useEffect(() => {
    backlogRef.current = backlog
    moveRef.current = move
  })

  useEffect(() => {
    return monitorForElements({
      canMonitor: ({ source }) => source.data.type === 'card',
      onDrop: ({ location, source }) => {
        const targets = location.current.dropTargets
        const listTarget = targets.find((t) => t.data.type === 'backlog-list')
        if (!listTarget) return

        const cardId = typeof source.data.cardId === 'string' ? source.data.cardId : ''
        const toListKey = (listTarget.data.listKey ?? null) as BacklogListKey

        const resolved = resolveBacklogDrop(backlogRef.current, cardId, toListKey)
        if (!resolved) return

        moveRef.current.mutate(resolved)
      },
    })
  }, [])

  return (
    <div className={styles.sections}>
      <BacklogList
        listKey={null}
        title="Backlog"
        cards={backlog.backlog}
        references={references}
        onOpen={onOpen}
        onOpenBoard={onOpenBoard}
      />
      {backlog.cycles.map((group) => {
        const subtitle = cycleSubtitle(group)
        return (
          <BacklogList
            key={group.cycle.id}
            listKey={group.cycle.id}
            title={group.cycle.name}
            {...(subtitle !== undefined && { subtitle })}
            cards={group.cards}
            references={references}
            onOpen={onOpen}
            onOpenBoard={onOpenBoard}
          />
        )
      })}
    </div>
  )
}

function cycleSubtitle(group: BacklogData['cycles'][number]): string | undefined {
  const parts: string[] = []
  if (group.cycle.state === 'active') parts.push('Active')
  if (group.cycle.startDate && group.cycle.endDate) {
    parts.push(`${group.cycle.startDate} – ${group.cycle.endDate}`)
  }
  if (group.cycle.goal) parts.push(group.cycle.goal)
  return parts.length > 0 ? parts.join(' · ') : undefined
}
