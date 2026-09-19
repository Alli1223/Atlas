import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import type { ProjectMember, Worklog as WorklogEntry, WorklogsResponse } from './api'
import { renderWithClient } from './test-support'
import { Worklog } from './Worklog'

const member: ProjectMember = {
  userId: '019f-user',
  username: 'ada',
  displayName: 'Ada Lovelace',
  avatarUrl: null,
  role: 'member',
  effectiveRole: 'member',
  instanceRole: 'member',
  isActive: true,
  addedAt: '2026-01-01T00:00:00Z',
  addedBy: null,
}

function entry(overrides: Partial<WorklogEntry> = {}): WorklogEntry {
  return {
    id: 'wl-1',
    cardId: 'card-1',
    authorId: member.userId,
    minutes: 150,
    note: null,
    source: 'manual',
    createdAt: '2026-01-02T00:00:00Z',
    ...overrides,
  }
}

describe('Worklog', () => {
  it('renders each entry with its formatted duration, author and total', async () => {
    const response: WorklogsResponse = {
      entries: [
        entry({ id: 'wl-2', minutes: 60, note: 'fixed it', source: 'smart-commit' }),
        entry({ id: 'wl-1', minutes: 150, note: null }),
      ],
      totalMinutes: 210,
    }
    stubFetch({ 'GET /api/v1/cards/ATLAS-1/worklogs': () => jsonResponse(response) })

    renderWithClient(<Worklog cardKey="ATLAS-1" members={[member]} />)

    expect(await screen.findByText('Time logged — 3h 30m')).toBeInTheDocument()
    expect(screen.getByText('1h')).toBeInTheDocument()
    expect(screen.getByText('2h 30m')).toBeInTheDocument()
    expect(screen.getByText('fixed it')).toBeInTheDocument()
    expect(screen.getByText('smart commit')).toBeInTheDocument()
    expect(screen.getAllByText('Ada Lovelace')).toHaveLength(2)
  })

  it('shows a placeholder rather than an empty list when nothing has been logged', async () => {
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/worklogs': () =>
        jsonResponse({ entries: [], totalMinutes: 0 } satisfies WorklogsResponse),
    })

    renderWithClient(<Worklog cardKey="ATLAS-1" members={[member]} />)

    expect(await screen.findByText('No time logged yet.')).toBeInTheDocument()
    expect(screen.getByText('Time logged')).toBeInTheDocument()
  })

  it('logs time through the composer and refetches the list', async () => {
    const user = userEvent.setup()
    let logged = false
    const { calls } = stubFetch({
      'GET /api/v1/cards/ATLAS-1/worklogs': () =>
        jsonResponse(
          logged
            ? { entries: [entry()], totalMinutes: 150 }
            : { entries: [], totalMinutes: 0 },
        ),
      'POST /api/v1/cards/ATLAS-1/worklogs': () => {
        logged = true
        return jsonResponse(entry(), 201)
      },
    })

    renderWithClient(<Worklog cardKey="ATLAS-1" members={[member]} />)
    await screen.findByText('No time logged yet.')

    await user.type(screen.getByLabelText('Duration'), '2h 30m')
    await user.type(screen.getByLabelText('Note'), 'wrote the fix')
    await user.click(screen.getByRole('button', { name: 'Log time' }))

    await waitFor(() => expect(calls).toContain('POST /api/v1/cards/ATLAS-1/worklogs'))
    expect(await screen.findByText('Time logged — 2h 30m')).toBeInTheDocument()
    // The composer clears on success rather than holding the just-submitted text.
    expect(screen.getByLabelText('Duration')).toHaveValue('')
    expect(screen.getByLabelText('Note')).toHaveValue('')
  })

  it('surfaces a rejected duration from the server rather than silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/worklogs': () =>
        jsonResponse({ entries: [], totalMinutes: 0 } satisfies WorklogsResponse),
      'POST /api/v1/cards/ATLAS-1/worklogs': () =>
        problemResponse('urn:atlas:validation', 422, 'the duration must be one or more tokens'),
    })

    renderWithClient(<Worklog cardKey="ATLAS-1" members={[member]} />)
    await screen.findByText('No time logged yet.')

    await user.type(screen.getByLabelText('Duration'), 'nonsense')
    await user.click(screen.getByRole('button', { name: 'Log time' }))

    expect(
      await screen.findByText('the duration must be one or more tokens'),
    ).toBeInTheDocument()
    // The composer keeps the rejected text rather than clearing it away.
    expect(screen.getByLabelText('Duration')).toHaveValue('nonsense')
  })
})
