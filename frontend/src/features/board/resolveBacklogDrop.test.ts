import { describe, expect, it } from 'vitest'

import type { BacklogCycle, BacklogData, BoardCard } from './api'
import { resolveBacklogDrop } from './resolveBacklogDrop'

function card(id: string): BoardCard {
  return {
    id,
    key: id.toUpperCase(),
    summary: id,
    typeId: 't',
    parentId: null,
    statusId: 's',
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    rank: id,
    estimate: null,
    tags: [],
    childRollup: null,
  }
}

function cycleGroup(id: string, ids: string[]): BacklogCycle {
  return {
    cycle: {
      id,
      projectId: 'p',
      name: id,
      goal: null,
      startDate: null,
      endDate: null,
      state: 'future',
      position: 0,
      createdAt: '',
      updatedAt: '',
    },
    cards: ids.map(card),
  }
}

function backlog(): BacklogData {
  return {
    backlog: [card('a'), card('b')],
    cycles: [cycleGroup('sprint-1', ['c']), cycleGroup('sprint-2', [])],
  }
}

describe('resolveBacklogDrop', () => {
  it('moves a backlog card into a cycle', () => {
    const move = resolveBacklogDrop(backlog(), 'a', 'sprint-1')
    expect(move).toEqual({ card: card('a'), toListKey: 'sprint-1' })
  })

  it('moves a cycle card back to the plain backlog', () => {
    const move = resolveBacklogDrop(backlog(), 'c', null)
    expect(move).toEqual({ card: card('c'), toListKey: null })
  })

  it('moves a card between two cycles', () => {
    const move = resolveBacklogDrop(backlog(), 'c', 'sprint-2')
    expect(move).toEqual({ card: card('c'), toListKey: 'sprint-2' })
  })

  it('is a no-op when dropped back onto its own list', () => {
    expect(resolveBacklogDrop(backlog(), 'a', null)).toBeNull()
    expect(resolveBacklogDrop(backlog(), 'c', 'sprint-1')).toBeNull()
  })

  it('is a no-op for a card not present anywhere in the backlog', () => {
    expect(resolveBacklogDrop(backlog(), 'ghost', 'sprint-1')).toBeNull()
  })
})
