import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import type { BoardCard as BoardCardData, CardType, Priority, UserDto } from './api'
import { BoardCard, type CardReferences } from './BoardCard'

const TYPE: CardType = {
  id: 'type-bug',
  projectId: 'p',
  name: 'Bug',
  icon: 'bug',
  colour: '#E5493A',
  level: 0,
  isDefault: false,
}

const PRIORITY: Priority = {
  id: 'pri-high',
  projectId: 'p',
  name: 'High',
  icon: 'chevron-up',
  colour: '#E9494A',
  rank: 2,
}

const USER: UserDto = {
  id: 'u1',
  username: 'alice',
  email: null,
  displayName: 'Alice Chen',
  avatarUrl: null,
  role: 'member',
  isActive: true,
  mustChangePassword: false,
  createdAt: '2026-07-17T00:00:00Z',
  updatedAt: '2026-07-17T00:00:00Z',
  lastLoginAt: null,
}

const references: CardReferences = {
  cardTypeById: new Map([[TYPE.id, TYPE]]),
  priorityById: new Map([[PRIORITY.id, PRIORITY]]),
  userById: new Map([[USER.id, USER]]),
}

function makeCard(overrides: Partial<BoardCardData> = {}): BoardCardData {
  return {
    id: 'c1',
    key: 'ATLAS-42',
    summary: 'Fix the drop indicator',
    typeId: TYPE.id,
    parentId: null,
    statusId: 'todo',
    priorityId: PRIORITY.id,
    assigneeId: USER.id,
    reporterId: null,
    rank: '8000',
    estimate: 5,
    tags: [{ id: 'tag1', projectId: 'p', name: 'bug', colour: 'red', createdAt: '2026-07-17T00:00:00Z' }],
    childRollup: null,
    ...overrides,
  }
}

describe('BoardCard', () => {
  it('renders the key, summary, tag, estimate and assignee', () => {
    render(<BoardCard card={makeCard()} laneKey="" references={references} onOpen={() => undefined} />)

    expect(screen.getByText('ATLAS-42')).toBeInTheDocument()
    expect(screen.getByText('Fix the drop indicator')).toBeInTheDocument()
    expect(screen.getByText('bug')).toBeInTheDocument()
    expect(screen.getByText('5')).toBeInTheDocument()
    // The assignee avatar carries the display name as its accessible label.
    expect(screen.getByLabelText('Alice Chen')).toBeInTheDocument()
  })

  it('marks an unassigned card as such rather than dropping the slot', () => {
    render(
      <BoardCard
        card={makeCard({ assigneeId: null })}
        laneKey=""
        references={references}
        onOpen={() => undefined}
      />,
    )
    expect(screen.getByLabelText('Unassigned')).toBeInTheDocument()
    expect(screen.queryByLabelText('Alice Chen')).not.toBeInTheDocument()
  })

  it('shows the mini-map only for a board-bearing card', () => {
    const { rerender } = render(
      <BoardCard card={makeCard()} laneKey="" references={references} onOpen={() => undefined} />,
    )
    expect(screen.queryByLabelText(/child cards done/)).not.toBeInTheDocument()

    rerender(
      <BoardCard
        card={makeCard({ childRollup: { total: 7, todo: 3, inProgress: 2, done: 2 } })}
        laneKey=""
        references={references}
        onOpen={() => undefined}
      />,
    )
    expect(screen.getByLabelText('2 of 7 child cards done')).toBeInTheDocument()
  })

  it('opens the card on click', async () => {
    const onOpen = vi.fn()
    render(<BoardCard card={makeCard()} laneKey="" references={references} onOpen={onOpen} />)
    await userEvent.click(screen.getByRole('button', { name: /ATLAS-42/ }))
    expect(onOpen).toHaveBeenCalledWith('ATLAS-42')
  })

  it('opens the nested board — not the card detail — when the mini-map is clicked', async () => {
    const onOpen = vi.fn()
    const onOpenBoard = vi.fn()
    render(
      <BoardCard
        card={makeCard({ childRollup: { total: 7, todo: 3, inProgress: 2, done: 2 } })}
        laneKey=""
        references={references}
        onOpen={onOpen}
        onOpenBoard={onOpenBoard}
      />,
    )

    await userEvent.click(screen.getByRole('button', { name: /Open ATLAS-42.*board/ }))

    // Drilling in is a distinct action from opening the detail: only onOpenBoard fires.
    expect(onOpenBoard).toHaveBeenCalledWith('ATLAS-42')
    expect(onOpen).not.toHaveBeenCalled()
  })

  it('shows the mini-map without a drill-in button when no board handler is given', () => {
    render(
      <BoardCard
        card={makeCard({ childRollup: { total: 3, todo: 3, inProgress: 0, done: 0 } })}
        laneKey=""
        references={references}
        onOpen={() => undefined}
      />,
    )
    // The mini-map still renders (the affordance that the card holds a board)...
    expect(screen.getByLabelText('0 of 3 child cards done')).toBeInTheDocument()
    // ...but there is no open-board button, since this board offers no drill-in target.
    expect(screen.queryByRole('button', { name: /Open ATLAS-42.*board/ })).not.toBeInTheDocument()
  })
})
