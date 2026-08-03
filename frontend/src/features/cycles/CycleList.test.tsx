import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import type { Cycle } from './api'
import { CycleList } from './CycleList'
import { makeCycle, renderWithClient } from './test-support'

afterEach(() => {
  vi.unstubAllGlobals()
})

function cyclesList(...cycles: Cycle[]) {
  return () => jsonResponse(cycles)
}

describe('CycleList', () => {
  it('shows an empty state and creates the first cycle', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(),
      'POST /api/v1/projects/ATLAS/cycles': () =>
        jsonResponse(makeCycle({ id: 'cycle-1', name: 'Sprint 1' })),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    expect(await screen.findByText('No cycles yet')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'New cycle' }))
    await user.type(screen.getByLabelText(/^Name/), 'Sprint 1')
    await user.click(screen.getByRole('button', { name: 'Create' }))

    await waitFor(() => expect(calls).toContain('POST /api/v1/projects/ATLAS/cycles'))
  })

  it('groups cycles by state', async () => {
    stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: '1', name: 'Sprint A', state: 'active' }),
        makeCycle({ id: '2', name: 'Sprint B', state: 'future' }),
        makeCycle({ id: '3', name: 'Sprint C', state: 'closed' }),
      ),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    const active = await screen.findByRole('region', { name: 'Active' })
    expect(within(active).getByText('Sprint A')).toBeInTheDocument()
    const future = screen.getByRole('region', { name: 'Future' })
    expect(within(future).getByText('Sprint B')).toBeInTheDocument()
    const closed = screen.getByRole('region', { name: 'Closed' })
    expect(within(closed).getByText('Sprint C')).toBeInTheDocument()
  })

  it('renames a cycle and edits its goal', async () => {
    const user = userEvent.setup()
    // A stateful stub: the rename invalidates the list, which refetches — a fixed response
    // would silently paper over a save that never actually reached the server.
    let cycle = makeCycle({ id: '1', name: 'Sprint A', state: 'future' })
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': () => jsonResponse([cycle]),
      'PATCH /api/v1/cycles/1': () => {
        cycle = { ...cycle, name: 'Sprint Alpha', goal: 'Ship it' }
        return jsonResponse(cycle)
      },
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    await screen.findByText('Sprint A')
    await user.click(screen.getByRole('button', { name: 'Edit' }))

    const nameField = screen.getByLabelText(/^Name/)
    await user.clear(nameField)
    await user.type(nameField, 'Sprint Alpha')
    await user.type(screen.getByLabelText('Goal'), 'Ship it')
    await user.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(calls).toContain('PATCH /api/v1/cycles/1'))
    expect(await screen.findByText('Sprint Alpha')).toBeInTheDocument()
  })

  it('starts a future cycle with both dates', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: '1', name: 'Sprint A', state: 'future' }),
      ),
      'POST /api/v1/cycles/1/start': () =>
        jsonResponse(
          makeCycle({
            id: '1',
            name: 'Sprint A',
            state: 'active',
            startDate: '2026-01-01',
            endDate: '2026-01-14',
          }),
        ),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    await user.click(await screen.findByRole('button', { name: 'Start' }))
    const dialog = await screen.findByRole('dialog')
    await user.type(within(dialog).getByLabelText(/^Start date/), '2026-01-01')
    await user.type(within(dialog).getByLabelText(/^End date/), '2026-01-14')
    await user.click(within(dialog).getByRole('button', { name: 'Start cycle' }))

    await waitFor(() => expect(calls).toContain('POST /api/v1/cycles/1/start'))
  })

  it('completes an active cycle, carrying incomplete cards to the backlog', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: '1', name: 'Sprint A', state: 'active' }),
      ),
      'POST /api/v1/cycles/1/complete': () =>
        jsonResponse(makeCycle({ id: '1', name: 'Sprint A', state: 'closed' })),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    await user.click(await screen.findByRole('button', { name: 'Complete' }))
    const dialog = await screen.findByRole('dialog')
    // "Move to the backlog" is the default selection.
    await user.click(within(dialog).getByRole('button', { name: 'Complete cycle' }))

    await waitFor(() => expect(calls).toContain('POST /api/v1/cycles/1/complete'))
  })

  it('reopens a closed cycle with a replanned end date', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({
          id: '1',
          name: 'Sprint A',
          state: 'closed',
          startDate: '2026-01-01',
          endDate: '2026-01-14',
        }),
      ),
      'POST /api/v1/cycles/1/reopen': () =>
        jsonResponse(
          makeCycle({
            id: '1',
            name: 'Sprint A',
            state: 'active',
            startDate: '2026-01-01',
            endDate: '2026-01-21',
          }),
        ),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    await user.click(await screen.findByRole('button', { name: 'Reopen' }))
    const dialog = await screen.findByRole('dialog')
    const endDateField = within(dialog).getByLabelText(/^New end date/)
    await user.clear(endDateField)
    await user.type(endDateField, '2026-01-21')
    await user.click(within(dialog).getByRole('button', { name: 'Reopen cycle' }))

    await waitFor(() => expect(calls).toContain('POST /api/v1/cycles/1/reopen'))
  })

  it('surfaces a failed create instead of silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(),
      'POST /api/v1/projects/ATLAS/cycles': () =>
        problemResponse('urn:atlas:error:validation', 422, 'Name must not be empty.'),
    })

    renderWithClient(<CycleList projectKey="ATLAS" />)

    await user.click(await screen.findByRole('button', { name: 'New cycle' }))
    await user.type(screen.getByLabelText(/^Name/), 'x')
    await user.click(screen.getByRole('button', { name: 'Create' }))

    expect(await screen.findByText('Name must not be empty.')).toBeInTheDocument()
  })
})
