import { monitorForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import { extractClosestEdge } from '@atlaskit/pragmatic-drag-and-drop-hitbox/closest-edge'
import { useEffect, useRef } from 'react'

import type { BoardData, BoardParams } from './api'
import type { CardReferences } from './BoardCard'
import { BoardColumnView } from './BoardColumnView'
import styles from './BoardView.module.css'
import { useMoveCard } from './queries'
import { resolveDrop } from './resolveDrop'
import { SwimlaneGroup } from './SwimlaneGroup'

/** Coerces an unknown PDND drag-data value to a string, the shape `resolveDrop` expects. */
function str(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

export interface BoardViewProps {
  projectKey: string
  params: BoardParams
  board: BoardData
  references: CardReferences
  /** WIP limits by status id, from the active saved board. */
  wipLimits: Record<string, number>
  onOpen: (cardKey: string) => void
  onOpenBoard: (cardKey: string) => void
}

/**
 * The board canvas: the columns (or swimlanes) plus the single global drag monitor that
 * turns every drop into an optimistic move.
 *
 * One monitor for the whole board, not one per card — a drop is resolved in exactly one
 * place. The monitor reads the *current* board from a ref (not a closure over the first
 * render's data) so a drop after several moves still resolves against the true state.
 */
export function BoardView({
  projectKey,
  params,
  board,
  references,
  wipLimits,
  onOpen,
  onOpenBoard,
}: BoardViewProps) {
  const move = useMoveCard(projectKey, params)

  // The monitor's callback outlives any single render; feed it fresh data through refs.
  // The refs are synced in an effect (not during render), so by the time an async drop
  // fires they already hold the latest board, names, and mutation.
  const boardRef = useRef(board)
  const statusNamesRef = useRef<Map<string, string>>(new Map())
  const moveRef = useRef(move)

  useEffect(() => {
    boardRef.current = board
    statusNamesRef.current = new Map(board.columns.map((c) => [c.status.id, c.status.name]))
    moveRef.current = move
  })

  useEffect(() => {
    return monitorForElements({
      canMonitor: ({ source }) => source.data.type === 'card',
      onDrop: ({ location, source }) => {
        const current = boardRef.current
        const targets = location.current.dropTargets
        if (targets.length === 0) return

        const cardTarget = targets.find((t) => t.data.type === 'card')
        const columnTarget = targets.find((t) => t.data.type === 'column')
        const targetData = cardTarget?.data ?? columnTarget?.data
        if (!targetData) return

        const resolved = resolveDrop(current, {
          cardId: str(source.data.cardId),
          sourceLaneKey: str(source.data.laneKey),
          targetStatusId: str(targetData.statusId),
          targetLaneKey: str(targetData.laneKey),
          ...(cardTarget ? { overCardId: str(cardTarget.data.cardId) } : {}),
          edge: cardTarget ? extractClosestEdge(cardTarget.data) : null,
        })
        if (!resolved) return

        const name = (id: string) => statusNamesRef.current.get(id) ?? 'another column'
        moveRef.current.mutate({
          ...resolved,
          fromStatusName: name(resolved.card.statusId),
          toStatusName: name(resolved.toStatusId),
        })
      },
    })
  }, [])

  if (board.swimlanes) {
    return (
      <div className={styles.swimlanes}>
        {board.swimlanes.map((lane) => (
          <SwimlaneGroup
            key={lane.key || '__none__'}
            lane={lane}
            references={references}
            wipLimits={wipLimits}
            onOpen={onOpen}
            onOpenBoard={onOpenBoard}
          />
        ))}
      </div>
    )
  }

  return (
    <div className={styles.columns}>
      {board.columns.map((column) => (
        <BoardColumnView
          key={column.status.id}
          column={column}
          laneKey=""
          references={references}
          onOpen={onOpen}
          onOpenBoard={onOpenBoard}
          {...(wipLimits[column.status.id] !== undefined
            ? { wipLimit: wipLimits[column.status.id] }
            : {})}
        />
      ))}
    </div>
  )
}
