import { produce } from 'immer'

import type { BoardCard, BoardColumn, BoardData } from './api'

/**
 * A resolved move: which card, to which column, to which index within it.
 *
 * `toIndex` is the destination index in the target column's list *after the card has been
 * removed from its source* — i.e. the final resting slot. `applyMove` and the neighbour
 * computation agree on that convention, which is the only way the optimistic placement and
 * the rank the server is asked for line up.
 */
export interface MoveIntent {
  cardId: string
  toStatusId: string
  toIndex: number
}

/** Locates a card within a set of columns. */
export function findCard(
  columns: BoardColumn[],
  cardId: string,
): { columnIndex: number; cardIndex: number } | null {
  for (let columnIndex = 0; columnIndex < columns.length; columnIndex += 1) {
    const cardIndex = columns[columnIndex]!.cards.findIndex((c) => c.id === cardId)
    if (cardIndex !== -1) return { columnIndex, cardIndex }
  }
  return null
}

/** Removes a card from one column array and inserts it into another at `toIndex`. Mutates. */
function relocate(columns: BoardColumn[], intent: MoveIntent): void {
  const found = findCard(columns, intent.cardId)
  if (!found) return
  const source = columns[found.columnIndex]!
  const [card] = source.cards.splice(found.cardIndex, 1)
  if (!card) return

  const target = columns.find((column) => column.status.id === intent.toStatusId)
  if (!target) {
    // The card's target column is not in this set of columns (a leaf swimlane that does not
    // hold this status). Put it back rather than dropping it on the floor.
    source.cards.splice(found.cardIndex, 0, card)
    return
  }

  card.statusId = intent.toStatusId
  const index = Math.max(0, Math.min(intent.toIndex, target.cards.length))
  target.cards.splice(index, 0, card)
}

/**
 * Applies a move to the board, immutably. Pure and total — an unknown card or target is a
 * no-op rather than a throw, because this runs inside a TanStack Query `setQueryData`
 * updater that may see a stale or half-loaded board.
 *
 * Both the flat `columns` and the lane that holds the card (when swimlanes are present) are
 * updated, so the optimistic move is correct whichever view is on screen. A status move
 * never changes a card's assignee or parent, so it never changes which lane it belongs to —
 * only its column within that lane.
 */
export function applyMove(board: BoardData, intent: MoveIntent): BoardData {
  return produce(board, (draft) => {
    relocate(draft.columns, intent)

    if (draft.swimlanes) {
      for (const lane of draft.swimlanes) {
        if (findCard(lane.columns, intent.cardId)) {
          relocate(lane.columns, intent)
          break
        }
      }
    }
  })
}

/**
 * The neighbours a card would have at `toIndex` in `cards`, once the card itself is removed.
 *
 * The board's move endpoint positions by neighbour ids (a fractional rank between two
 * cards), not by an integer index — so a concurrent move by someone else does not silently
 * renumber. This translates the drop index into that pair.
 */
export function neighboursAt(
  cards: BoardCard[],
  toIndex: number,
  movedCardId: string,
): { previousCardId?: string; nextCardId?: string } {
  const without = cards.filter((c) => c.id !== movedCardId)
  const clamped = Math.max(0, Math.min(toIndex, without.length))
  const previous = without[clamped - 1]
  const next = without[clamped]
  return {
    ...(previous ? { previousCardId: previous.id } : {}),
    ...(next ? { nextCardId: next.id } : {}),
  }
}
