import { describe, expect, it } from 'vitest'

import type { BoardCard, BoardColumn, BoardData } from './api'
import { resolveDrop } from './resolveDrop'

function card(id: string, statusId: string): BoardCard {
  return {
    id,
    key: id.toUpperCase(),
    summary: id,
    typeId: 't',
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

function column(statusId: string, ids: string[]): BoardColumn {
  return { status: { id: statusId, name: statusId, category: 'todo' }, cards: ids.map((i) => card(i, statusId)) }
}

function board(): BoardData {
  return { columns: [column('todo', ['a', 'b', 'c']), column('doing', ['d'])] }
}

describe('resolveDrop', () => {
  it('drops onto the top edge of a card in another column', () => {
    const move = resolveDrop(board(), {
      cardId: 'a',
      sourceLaneKey: '',
      targetStatusId: 'doing',
      targetLaneKey: '',
      overCardId: 'd',
      edge: 'top',
    })
    expect(move).toMatchObject({ toStatusId: 'doing', toIndex: 0, sameColumn: false, nextCardId: 'd' })
    expect(move?.previousCardId).toBeUndefined()
  })

  it('drops onto the bottom edge past the last card', () => {
    const move = resolveDrop(board(), {
      cardId: 'a',
      sourceLaneKey: '',
      targetStatusId: 'doing',
      targetLaneKey: '',
      overCardId: 'd',
      edge: 'bottom',
    })
    expect(move).toMatchObject({ toStatusId: 'doing', toIndex: 1, previousCardId: 'd' })
  })

  it('drops on the empty column body at the end', () => {
    const empty: BoardData = { columns: [column('todo', ['a']), column('doing', [])] }
    const move = resolveDrop(empty, {
      cardId: 'a',
      sourceLaneKey: '',
      targetStatusId: 'doing',
      targetLaneKey: '',
    })
    expect(move).toMatchObject({ toStatusId: 'doing', toIndex: 0, sameColumn: false })
    expect(move?.previousCardId).toBeUndefined()
    expect(move?.nextCardId).toBeUndefined()
  })

  it('reorders within the same column and adjusts for the removed source', () => {
    // Drag `a` onto the bottom of `b`: post-removal target [b, c], land after b → index 1.
    const move = resolveDrop(board(), {
      cardId: 'a',
      sourceLaneKey: '',
      targetStatusId: 'todo',
      targetLaneKey: '',
      overCardId: 'b',
      edge: 'bottom',
    })
    expect(move).toMatchObject({ sameColumn: true, toIndex: 1, previousCardId: 'b', nextCardId: 'c' })
  })

  it('returns null when the drop changes nothing', () => {
    // Dropping `b` on the top of `b` is a no-op.
    const move = resolveDrop(board(), {
      cardId: 'b',
      sourceLaneKey: '',
      targetStatusId: 'todo',
      targetLaneKey: '',
      overCardId: 'b',
      edge: 'top',
    })
    expect(move).toBeNull()
  })

  it('ignores a same-status drop into a different lane (would be a reassignment)', () => {
    const withLanes: BoardData = {
      ...board(),
      swimlanes: [
        { key: 'u1', label: 'A', columns: [column('todo', ['a']), column('doing', [])] },
        { key: 'u2', label: 'B', columns: [column('todo', ['b', 'c']), column('doing', ['d'])] },
      ],
    }
    const move = resolveDrop(withLanes, {
      cardId: 'a',
      sourceLaneKey: 'u1',
      targetStatusId: 'todo',
      targetLaneKey: 'u2',
      overCardId: 'b',
      edge: 'top',
    })
    expect(move).toBeNull()
  })

  it('is null for an unknown card', () => {
    expect(
      resolveDrop(board(), { cardId: 'zzz', sourceLaneKey: '', targetStatusId: 'doing', targetLaneKey: '' }),
    ).toBeNull()
  })
})
