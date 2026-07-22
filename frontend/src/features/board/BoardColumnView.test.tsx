import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { BoardCard as BoardCardData, BoardColumn } from './api'
import { type CardReferences } from './BoardCard'
import { BoardColumnView } from './BoardColumnView'

const references: CardReferences = {
  cardTypeById: new Map(),
  priorityById: new Map(),
  userById: new Map(),
}

function makeCard(id: string): BoardCardData {
  return {
    id,
    key: id.toUpperCase(),
    summary: id,
    typeId: 't',
    parentId: null,
    statusId: 'prog',
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    rank: id,
    estimate: null,
    tags: [],
    childRollup: null,
  }
}

function column(ids: string[]): BoardColumn {
  return {
    status: { id: 'prog', name: 'In Progress', category: 'in_progress' },
    cards: ids.map(makeCard),
  }
}

const noop = () => undefined

describe('BoardColumnView', () => {
  it('renders the status name and the plain count when no WIP limit is set', () => {
    render(
      <BoardColumnView
        column={column(['a', 'b'])}
        laneKey=""
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
      />,
    )
    expect(screen.getByText('In Progress')).toBeInTheDocument()
    expect(screen.getByText('2')).toBeInTheDocument()
  })

  it('shows count-over-limit and flags a breach when the WIP limit is exceeded', () => {
    render(
      <BoardColumnView
        column={column(['a', 'b', 'c'])}
        laneKey=""
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
        wipLimit={2}
      />,
    )
    expect(screen.getByText('3/2')).toBeInTheDocument()
    const header = screen.getByText('In Progress').closest('header')
    expect(header).toHaveAttribute('data-wip-breached', 'true')
  })

  it('does not flag a breach at or under the limit', () => {
    render(
      <BoardColumnView
        column={column(['a', 'b'])}
        laneKey=""
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
        wipLimit={2}
      />,
    )
    expect(screen.getByText('2/2')).toBeInTheDocument()
    const header = screen.getByText('In Progress').closest('header')
    expect(header).toHaveAttribute('data-wip-breached', 'false')
  })
})
