import { describe, expect, it } from 'vitest'

import type { BoardCard, BoardColumn, BoardData } from './api'
import { applyMove, findCard, neighboursAt } from './applyMove'

function card(id: string, statusId: string): BoardCard {
  return {
    id,
    key: id.toUpperCase(),
    summary: `card ${id}`,
    typeId: 'type-1',
    parentId: null,
    statusId,
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    rank: id,
    estimate: null,
    tags: [],
    childRollup: null,
  }
}

function column(statusId: string, name: string, ids: string[]): BoardColumn {
  return {
    status: { id: statusId, name, category: 'todo' },
    cards: ids.map((id) => card(id, statusId)),
  }
}

/** A two-column board: To Do [a,b,c], Doing [d]. */
function board(): BoardData {
  return {
    columns: [column('todo', 'To Do', ['a', 'b', 'c']), column('doing', 'Doing', ['d'])],
  }
}

const ids = (col: BoardColumn) => col.cards.map((c) => c.id)

describe('applyMove', () => {
  it('moves a card to another column at the requested index and updates its status', () => {
    const next = applyMove(board(), { cardId: 'a', toStatusId: 'doing', toIndex: 1 })
    expect(ids(next.columns[0]!)).toEqual(['b', 'c'])
    expect(ids(next.columns[1]!)).toEqual(['d', 'a'])
    expect(findCard(next.columns, 'a')).toEqual({ columnIndex: 1, cardIndex: 1 })
    expect(next.columns[1]!.cards[1]!.statusId).toBe('doing')
  })

  it('reorders within the same column using the post-removal index', () => {
    // Move `a` (index 0) to sit after `b`: post-removal target list is [b, c], index 1.
    const next = applyMove(board(), { cardId: 'a', toStatusId: 'todo', toIndex: 1 })
    expect(ids(next.columns[0]!)).toEqual(['b', 'a', 'c'])
  })

  it('does not mutate the input board', () => {
    const original = board()
    applyMove(original, { cardId: 'a', toStatusId: 'doing', toIndex: 0 })
    expect(ids(original.columns[0]!)).toEqual(['a', 'b', 'c'])
    expect(ids(original.columns[1]!)).toEqual(['d'])
  })

  it('is a no-op for an unknown card', () => {
    const next = applyMove(board(), { cardId: 'zzz', toStatusId: 'doing', toIndex: 0 })
    expect(ids(next.columns[0]!)).toEqual(['a', 'b', 'c'])
    expect(ids(next.columns[1]!)).toEqual(['d'])
  })

  it('updates the swimlane that holds the card as well as the flat columns', () => {
    const withLanes: BoardData = {
      ...board(),
      swimlanes: [
        {
          key: 'u1',
          label: 'Alice',
          columns: [column('todo', 'To Do', ['a', 'b']), column('doing', 'Doing', [])],
        },
        {
          key: 'u2',
          label: 'Bob',
          columns: [column('todo', 'To Do', ['c']), column('doing', 'Doing', ['d'])],
        },
      ],
    }
    const next = applyMove(withLanes, { cardId: 'a', toStatusId: 'doing', toIndex: 0 })
    // Flat board updated...
    expect(ids(next.columns[1]!)).toContain('a')
    // ...and Alice's lane updated, Bob's lane untouched.
    expect(ids(next.swimlanes![0]!.columns[1]!)).toEqual(['a'])
    expect(ids(next.swimlanes![1]!.columns[0]!)).toEqual(['c'])
  })
})

describe('neighboursAt', () => {
  const list = [card('a', 'todo'), card('b', 'todo'), card('c', 'todo')]

  it('names both neighbours in the middle', () => {
    expect(neighboursAt(list, 1, 'x')).toEqual({ previousCardId: 'a', nextCardId: 'b' })
  })

  it('omits the previous at the top and the next at the bottom', () => {
    expect(neighboursAt(list, 0, 'x')).toEqual({ nextCardId: 'a' })
    expect(neighboursAt(list, 3, 'x')).toEqual({ previousCardId: 'c' })
  })

  it('ignores the moved card when computing neighbours', () => {
    // Placing `b` at index 1 of the list-without-b: neighbours are a and c, never b itself.
    expect(neighboursAt(list, 1, 'b')).toEqual({ previousCardId: 'a', nextCardId: 'c' })
  })
})
