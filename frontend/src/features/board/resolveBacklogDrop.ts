import type { BacklogData, BoardCard } from './api'
import type { BacklogListKey } from './queries'

/** A resolved backlog drop: the card, and the list it should end up in. */
export interface ResolvedBacklogDrop {
  card: BoardCard
  toListKey: BacklogListKey
}

/** Which list currently holds a card — the plain backlog (`null`) or a cycle id. */
function currentListOf(backlog: BacklogData, cardId: string): BacklogListKey | undefined {
  if (backlog.backlog.some((c) => c.id === cardId)) return null
  const group = backlog.cycles.find((g) => g.cards.some((c) => c.id === cardId))
  return group ? group.cycle.id : undefined
}

/**
 * Turns a raw backlog drop (a card id and a target list key) into a resolved move, or `null`
 * when there is nothing to do.
 *
 * Pure and total, the same reason `resolveDrop` is for the main board: PDND's drag events do
 * not exist in jsdom, so this is where the logic actually lives and is unit-tested, and
 * `BacklogView`'s monitor is left as thin, untested glue around it.
 *
 * Unlike `resolveDrop`, there is no rank to resolve — a card dropped anywhere in a list just
 * joins (or leaves) that list's cycle, so the only questions are "which card" and "which
 * list", never "where in it".
 */
export function resolveBacklogDrop(
  backlog: BacklogData,
  cardId: string,
  toListKey: BacklogListKey,
): ResolvedBacklogDrop | null {
  const fromListKey = currentListOf(backlog, cardId)
  // An unknown card (stale drag data, or a board that moved on mid-drag) or a drop back onto
  // its own list is not a move.
  if (fromListKey === undefined || fromListKey === toListKey) return null

  const card =
    backlog.backlog.find((c) => c.id === cardId) ??
    backlog.cycles.flatMap((g) => g.cards).find((c) => c.id === cardId)
  if (!card) return null

  return { card, toListKey }
}
