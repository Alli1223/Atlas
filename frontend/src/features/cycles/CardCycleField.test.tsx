import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import type { Cycle } from './api'
import { CardCycleField } from './CardCycleField'
import { makeCycle, renderWithClient } from './test-support'

afterEach(() => {
  vi.unstubAllGlobals()
})

const NOT_FOUND = 'urn:atlas:error:not-found'

function project(overrides: Record<string, unknown> = {}) {
  return {
    id: 'project-1',
    key: 'ATLAS',
    name: 'Atlas',
    template: 'programming',
    cardCounter: 1,
    cyclesEnabled: true,
    estimationUnit: 'none',
    createdAt: '2026-07-01T00:00:00Z',
    updatedAt: '2026-07-01T00:00:00Z',
    ...overrides,
  }
}

function cyclesList(...cycles: Cycle[]) {
  return () => jsonResponse(cycles)
}

describe('CardCycleField', () => {
  it('renders nothing when the project has not turned cycles on', async () => {
    stubFetch({
      'GET /api/v1/projects/ATLAS': () => jsonResponse(project({ cyclesEnabled: false })),
      'GET /api/v1/cards/ATLAS-1/cycle': () => problemResponse(NOT_FOUND, 404),
    })

    const { container } = renderWithClient(
      <CardCycleField cardKey="ATLAS-1" projectKey="ATLAS" />,
    )

    // Nothing to wait on other than the fact it never renders — give the queries a tick.
    await waitFor(() => expect(screen.queryByText('Cycle')).not.toBeInTheDocument())
    expect(container).toBeEmptyDOMElement()
  })

  it('offers a picker of open cycles when the card is not in one', async () => {
    stubFetch({
      'GET /api/v1/projects/ATLAS': () => jsonResponse(project()),
      'GET /api/v1/cards/ATLAS-1/cycle': () => problemResponse(NOT_FOUND, 404),
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: 'cycle-1', name: 'Sprint 1', state: 'future' }),
        makeCycle({ id: 'cycle-2', name: 'Sprint 0', state: 'closed' }),
      ),
    })

    renderWithClient(<CardCycleField cardKey="ATLAS-1" projectKey="ATLAS" />)

    expect(await screen.findByRole('option', { name: 'Sprint 1 (Future)' })).toBeInTheDocument()
    // A closed cycle cannot be added to, so it is not offered.
    expect(screen.queryByRole('option', { name: /Sprint 0/ })).not.toBeInTheDocument()
  })

  it('adds the card to the chosen cycle', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS': () => jsonResponse(project()),
      'GET /api/v1/cards/ATLAS-1/cycle': () => problemResponse(NOT_FOUND, 404),
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: 'cycle-1', name: 'Sprint 1', state: 'future' }),
      ),
      'POST /api/v1/cards/ATLAS-1/cycle': () => new Response(null, { status: 204 }),
    })

    renderWithClient(<CardCycleField cardKey="ATLAS-1" projectKey="ATLAS" />)

    const select = await screen.findByRole('combobox', { name: 'Add to cycle' })
    await user.selectOptions(select, 'cycle-1')

    await waitFor(() => expect(calls).toContain('POST /api/v1/cards/ATLAS-1/cycle'))
  })

  it('shows the current cycle and its goal, and removes it on click', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS': () => jsonResponse(project()),
      'GET /api/v1/cards/ATLAS-1/cycle': () =>
        jsonResponse(
          makeCycle({ id: 'cycle-1', name: 'Sprint 1', state: 'active', goal: 'Ship it' }),
        ),
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(),
      'DELETE /api/v1/cards/ATLAS-1/cycle': () => new Response(null, { status: 204 }),
    })

    renderWithClient(<CardCycleField cardKey="ATLAS-1" projectKey="ATLAS" />)

    expect(await screen.findByText('Sprint 1')).toBeInTheDocument()
    expect(screen.getByText('Ship it')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Remove' }))
    await waitFor(() => expect(calls).toContain('DELETE /api/v1/cards/ATLAS-1/cycle'))
  })

  it('surfaces a failed add instead of silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/projects/ATLAS': () => jsonResponse(project()),
      'GET /api/v1/cards/ATLAS-1/cycle': () => problemResponse(NOT_FOUND, 404),
      'GET /api/v1/projects/ATLAS/cycles': cyclesList(
        makeCycle({ id: 'cycle-1', name: 'Sprint 1', state: 'future' }),
      ),
      'POST /api/v1/cards/ATLAS-1/cycle': () =>
        problemResponse('urn:atlas:error:conflict', 409, 'the cycle is closed'),
    })

    renderWithClient(<CardCycleField cardKey="ATLAS-1" projectKey="ATLAS" />)

    const select = await screen.findByRole('combobox', { name: 'Add to cycle' })
    await user.selectOptions(select, 'cycle-1')

    expect(await screen.findByText('the cycle is closed')).toBeInTheDocument()
  })
})
