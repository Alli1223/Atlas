import { screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { jsonResponse, stubFetch } from '@/features/auth/test-support'

import type { HistoryEntry, ProjectMember } from './api'
import { HistoryTab } from './HistoryTab'
import { renderWithClient } from './test-support'

const MEMBERS: ProjectMember[] = [
  {
    userId: 'user-alice',
    username: 'alice',
    displayName: 'Alice Ng',
    avatarUrl: null,
    role: 'member',
    effectiveRole: 'member',
    instanceRole: 'member',
    isActive: true,
    addedAt: '2026-07-01T00:00:00Z',
    addedBy: null,
  },
]

function entry(overrides: Partial<HistoryEntry>): HistoryEntry {
  return {
    id: crypto.randomUUID(),
    cardId: 'card-1',
    authorId: 'user-alice',
    createdAt: '2026-07-16T12:00:00Z',
    field: 'status',
    fromValue: null,
    fromDisplay: null,
    toValue: null,
    toDisplay: null,
    ...overrides,
  }
}

describe('HistoryTab', () => {
  it('renders each field change with who, the field, and from → to', async () => {
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/history': () =>
        jsonResponse([
          entry({ field: 'status', fromDisplay: 'To Do', toDisplay: 'In Progress' }),
          entry({ field: 'assignee', fromDisplay: null, toDisplay: 'Alice Ng' }),
        ]),
    })

    renderWithClient(<HistoryTab cardKey="ATLAS-1" members={MEMBERS} enabled />)

    // The changed values are shown as captured, not as raw ids.
    expect(await screen.findByText('To Do')).toBeInTheDocument()
    expect(screen.getByText('In Progress')).toBeInTheDocument()
    // Field names are humanised, and the author is resolved from the member list.
    expect(screen.getByText('Status')).toBeInTheDocument()
    expect(screen.getByText('Assignee')).toBeInTheDocument()
    expect(screen.getAllByText('Alice Ng').length).toBeGreaterThan(0)
  })

  it('shows an empty state when nothing has changed', async () => {
    stubFetch({ 'GET /api/v1/cards/ATLAS-1/history': () => jsonResponse([]) })
    renderWithClient(<HistoryTab cardKey="ATLAS-1" members={MEMBERS} enabled />)
    expect(await screen.findByText('No changes recorded yet.')).toBeInTheDocument()
  })

  it('does not fetch history until the tab is enabled', () => {
    const { calls } = stubFetch({
      'GET /api/v1/cards/ATLAS-1/history': () => jsonResponse([]),
    })
    renderWithClient(<HistoryTab cardKey="ATLAS-1" members={MEMBERS} enabled={false} />)
    // A disabled query must not hit the network — opening a card should not pay for history.
    expect(calls).not.toContain('GET /api/v1/cards/ATLAS-1/history')
  })
})
