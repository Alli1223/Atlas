import type { BoardCard, BoardColumn, BoardData } from './api'
import { findCard, neighboursAt } from './applyMove'

/** A closest-edge value from the hitbox package. */
export type Edge = 'top' | 'bottom' | 'left' | 'right' | null

/** Everything a drop needs resolving, gathered from the PDND source and target data. */
export interface DropInput {
  cardId: string
  /** The lane the dragged card came from — `''` for the flat board. */
  sourceLaneKey: string
  /** The status column being dropped into. */
  targetStatusId: string
  /** The lane being dropped into — `''` for the flat board. */
  targetLaneKey: string
  /** The card the pointer is over, if the drop landed on a card rather than empty column. */
  overCardId?: string
  /** Which edge of `overCardId` the pointer is nearer. */
  edge?: Edge
}

/** A resolved drop, minus the human status names the mutation adds for its toast. */
export interface ResolvedDrop {
  card: BoardCard
  toStatusId: string
  toIndex: number
  sameColumn: boolean
  previousCardId?: string
  nextCardId?: string
}

/** The rendered card list for a status column, in the flat board or within a lane. */
function columnCards(board: BoardData, statusId: string, laneKey: string): BoardCard[] | null {
  const columns: BoardColumn[] | undefined =
    laneKey !== '' && board.swimlanes
      ? board.swimlanes.find((lane) => lane.key === laneKey)?.columns
      : board.columns
  return columns?.find((c) => c.status.id === statusId)?.cards ?? null
}

/**
 * Turns a raw drop (source + target data from PDND) into a structural move, or `null` when
 * there is nothing to do.
 *
 * Pure and total so it is exhaustively unit-testable without a browser — PDND's drag events
 * do not exist in jsdom, so this is where the reorder maths lives and is tested, not in the
 * DOM. The `toIndex` it returns is the destination index *after the card is removed from its
 * source*, the convention `applyMove` and `neighboursAt` share.
 *
 * Cross-lane same-status drops resolve to `null`: moving a card between an "Alice" and a
 * "Bob" assignee lane would be a reassignment, which is not what a status/rank move does —
 * so rather than silently snapping it back, the drop is ignored.
 */
export function resolveDrop(board: BoardData, input: DropInput): ResolvedDrop | null {
  const located = findCard(board.columns, input.cardId)
  if (!located) return null
  const card = board.columns[located.columnIndex]!.cards[located.cardIndex]!

  const sameStatus = card.statusId === input.targetStatusId
  const sameLane = input.targetLaneKey === input.sourceLaneKey

  // A same-status drop into a different lane would require reassigning the card; that is not
  // a move. Ignore rather than snap back.
  if (sameStatus && !sameLane) return null

  const targetCards = columnCards(board, input.targetStatusId, input.targetLaneKey)
  if (!targetCards) return null

  // The insertion index among the target column's rendered cards.
  let index: number
  if (input.overCardId !== undefined) {
    const overIndex = targetCards.findIndex((c) => c.id === input.overCardId)
    index =
      overIndex === -1 ? targetCards.length : overIndex + (input.edge === 'bottom' ? 1 : 0)
  } else {
    index = targetCards.length
  }

  // Adjust to the post-removal index when the source lives in this same list.
  const sourceIndex = targetCards.findIndex((c) => c.id === input.cardId)
  if (sourceIndex !== -1 && sourceIndex < index) index -= 1

  const sameColumn = sameStatus && sameLane

  if (sameColumn) {
    // A drop that changes nothing is not a move.
    const without = targetCards.filter((c) => c.id !== input.cardId)
    const result = [...without.slice(0, index), card, ...without.slice(index)]
    const unchanged =
      result.length === targetCards.length &&
      result.every((c, i) => c.id === targetCards[i]!.id)
    if (unchanged) return null
  }

  const neighbours = neighboursAt(targetCards, index, input.cardId)

  return {
    card,
    toStatusId: input.targetStatusId,
    toIndex: index,
    sameColumn,
    ...neighbours,
  }
}
