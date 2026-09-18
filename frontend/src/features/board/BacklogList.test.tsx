import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { BoardCard as BoardCardData } from './api'
import { BacklogList } from './BacklogList'
import { type CardReferences } from './BoardCard'

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

const noop = () => undefined

describe('BacklogList', () => {
  it('renders the title, subtitle and count', () => {
    render(
      <BacklogList
        listKey="sprint-1"
        title="Sprint 1"
        subtitle="Active"
        cards={[makeCard('a'), makeCard('b')]}
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
      />,
    )
    expect(screen.getByText('Sprint 1')).toBeInTheDocument()
    expect(screen.getByText('Active')).toBeInTheDocument()
    expect(screen.getByText('2')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'A: a' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'B: b' })).toBeInTheDocument()
  })

  it('shows an empty-drop-zone placeholder rather than nothing when there are no cards', () => {
    render(
      <BacklogList
        listKey={null}
        title="Backlog"
        cards={[]}
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
      />,
    )
    expect(screen.getByText('Backlog')).toBeInTheDocument()
    expect(screen.getByText('0')).toBeInTheDocument()
    expect(screen.getByText('Drop a card here')).toBeInTheDocument()
  })

  it('renders with no subtitle at all when none is given', () => {
    render(
      <BacklogList
        listKey={null}
        title="Backlog"
        cards={[]}
        references={references}
        onOpen={noop}
        onOpenBoard={noop}
      />,
    )
    expect(screen.queryByText('Active')).not.toBeInTheDocument()
  })
})
