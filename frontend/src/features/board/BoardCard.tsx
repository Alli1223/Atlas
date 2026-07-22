import { combine } from '@atlaskit/pragmatic-drag-and-drop/combine'
import {
  draggable,
  dropTargetForElements,
} from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import {
  attachClosestEdge,
  extractClosestEdge,
} from '@atlaskit/pragmatic-drag-and-drop-hitbox/closest-edge'
import { DropIndicator } from '@atlaskit/pragmatic-drag-and-drop-react-drop-indicator/box'
import { useEffect, useRef, useState } from 'react'

import { Avatar, Tag } from '@/components/ui'
import { cx } from '@/lib/cx'

import type { CardType, Priority, UserDto } from './api'
import type { BoardCard as BoardCardData } from './api'
import styles from './BoardCard.module.css'
import { CardMiniMap } from './CardMiniMap'
import { cardTypeColour, cardTypeIcon, Glyph, priorityColour, priorityIcon } from './icons'
import type { Edge } from './resolveDrop'

export interface CardReferences {
  cardTypeById: Map<string, CardType>
  priorityById: Map<string, Priority>
  userById: Map<string, UserDto>
}

export interface BoardCardProps {
  card: BoardCardData
  /** The lane this instance is rendered in — `''` for the flat board. Rides the drag data. */
  laneKey: string
  references: CardReferences
  /** Opens the card's detail. The board route turns this into a `?card=KEY` navigation. */
  onOpen: (cardKey: string) => void
  /** Opens the card *as a board* (its children). Provided only for board-bearing cards. */
  onOpenBoard?: (cardKey: string) => void
}

/**
 * One board card — the signature surface of the whole app.
 *
 * It is both a `draggable` and a `dropTargetForElements` with top/bottom edge detection, so
 * a card can be picked up and other cards can be dropped above or below it. The single
 * global monitor in `BoardView` turns the drop into the optimistic move; this component only
 * reports *where* a drop would land (the edge) and renders the indicator there.
 */
export function BoardCard({ card, laneKey, references, onOpen, onOpenBoard }: BoardCardProps) {
  const ref = useRef<HTMLDivElement>(null)
  const [dragging, setDragging] = useState(false)
  const [edge, setEdge] = useState<Edge>(null)
  // A drag ends with a click event on some browsers; this suppresses opening the card then.
  const draggedRef = useRef(false)

  useEffect(() => {
    const element = ref.current
    if (!element) return

    const data = { type: 'card', cardId: card.id, statusId: card.statusId, laneKey }

    return combine(
      draggable({
        element,
        getInitialData: () => data,
        onDragStart: () => {
          draggedRef.current = true
          setDragging(true)
        },
        onDrop: () => setDragging(false),
      }),
      dropTargetForElements({
        element,
        canDrop: ({ source }) => source.data.type === 'card',
        getData: ({ input }) =>
          attachClosestEdge(data, { element, input, allowedEdges: ['top', 'bottom'] }),
        onDrag: ({ self }) => setEdge(extractClosestEdge(self.data)),
        onDragLeave: () => setEdge(null),
        onDrop: () => setEdge(null),
      }),
    )
  }, [card.id, card.statusId, laneKey])

  const type = references.cardTypeById.get(card.typeId)
  const priority = card.priorityId != null ? references.priorityById.get(card.priorityId) : undefined
  const assignee = card.assigneeId != null ? references.userById.get(card.assigneeId) : undefined

  return (
    <div
      ref={ref}
      className={cx(styles.card, dragging && styles.dragging, card.childRollup != null && styles.boardBearing)}
      role="button"
      tabIndex={0}
      aria-label={`${card.key}: ${card.summary}`}
      onClick={() => {
        if (draggedRef.current) {
          draggedRef.current = false
          return
        }
        onOpen(card.key)
      }}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          onOpen(card.key)
        }
      }}
    >
      {card.tags.length > 0 && (
        <div className={styles.tags}>
          {card.tags.map((tag) => (
            <Tag key={tag.id} color={tag.colour ?? 'standard'}>
              {tag.name}
            </Tag>
          ))}
        </div>
      )}

      <p className={styles.summary}>{card.summary}</p>

      {card.childRollup &&
        (onOpenBoard ? (
          <button
            type="button"
            className={styles.miniMapButton}
            onClick={(event) => {
              event.stopPropagation()
              onOpenBoard(card.key)
            }}
            title={`Open ${card.key}'s board`}
            aria-label={`Open ${card.key}'s board`}
          >
            <CardMiniMap rollup={card.childRollup} />
          </button>
        ) : (
          <CardMiniMap rollup={card.childRollup} />
        ))}

      <div className={styles.footer}>
        <div className={styles.footerLeft}>
          <span
            className={styles.typeIcon}
            style={{ color: cardTypeColour(type) }}
            title={type?.name ?? 'Card'}
          >
            <Glyph icon={cardTypeIcon(type)} size={16} strokeWidth={2.25} aria-hidden="true" />
          </span>
          <span className={styles.key}>{card.key}</span>
          {priority && (
            <span
              className={styles.priorityIcon}
              style={{ color: priorityColour(priority) }}
              title={`${priority.name} priority`}
            >
              <Glyph icon={priorityIcon(priority)} size={16} strokeWidth={2.5} aria-hidden="true" />
            </span>
          )}
        </div>

        <div className={styles.footerRight}>
          {card.estimate != null && (
            <span className={styles.estimate} title="Estimate">
              {card.estimate}
            </span>
          )}
          {assignee ? (
            <Avatar
              name={assignee.displayName}
              {...(assignee.avatarUrl != null ? { src: assignee.avatarUrl } : {})}
              size="small"
            />
          ) : (
            <span className={styles.unassigned} aria-label="Unassigned" title="Unassigned" />
          )}
        </div>
      </div>

      {edge && <DropIndicator edge={edge} gap="8px" />}
    </div>
  )
}
