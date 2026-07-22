import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { jsonResponse, stubFetch } from '@/features/auth/test-support'

import type { AvailableTransition } from './api'
import { makeCard } from './test-support'
import { renderWithClient } from './test-support'
import { TransitionButtons } from './TransitionButtons'

/**
 * The board must only ever offer a move the card can actually make. The backend has already
 * dropped the transitions whose conditions fail, so the component's job is to render *exactly*
 * that list and invent nothing — this suite pins that it renders precisely the endpoint's set,
 * and that a transition the endpoint withheld never appears as a button.
 */
describe('TransitionButtons', () => {
  it('renders exactly the transitions the endpoint returns as buttons', async () => {
    // The workflow defines "Start progress", "Block", and "Done" — but the card's condition
    // for "Block" fails, so the endpoint returns only the other two. "Block" must not appear.
    const available: AvailableTransition[] = [
      { id: 't-start', name: 'Start progress', toStatusId: 'status-inprogress' },
      { id: 't-done', name: 'Done', toStatusId: 'status-done' },
    ]
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/transitions': () => jsonResponse(available),
    })

    renderWithClient(<TransitionButtons cardKey="ATLAS-1" />)

    expect(await screen.findByRole('button', { name: 'Start progress' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Done' })).toBeInTheDocument()
    // The condition-blocked transition is absent — not disabled, absent.
    expect(screen.queryByRole('button', { name: 'Block' })).not.toBeInTheDocument()
    // Precisely two moves, no more.
    expect(screen.getByRole('group', { name: 'Move this card' }).querySelectorAll('button')).toHaveLength(
      2,
    )
  })

  it('executes a transition by its id when its button is pressed', async () => {
    const { calls } = stubFetch({
      'GET /api/v1/cards/ATLAS-1/transitions': () =>
        jsonResponse([{ id: 't-done', name: 'Done', toStatusId: 'status-done' }]),
      'POST /api/v1/cards/ATLAS-1/transitions/t-done': () =>
        jsonResponse(makeCard({ statusId: 'status-done' })),
      'GET /api/v1/cards/ATLAS-1': () => jsonResponse(makeCard()),
      'GET /api/v1/cards/ATLAS-1/history': () => jsonResponse([]),
    })

    renderWithClient(<TransitionButtons cardKey="ATLAS-1" />)
    await userEvent.click(await screen.findByRole('button', { name: 'Done' }))

    // The named transition is executed through its own endpoint, not a blind status write.
    await waitFor(() =>
      expect(calls).toContain('POST /api/v1/cards/ATLAS-1/transitions/t-done'),
    )
  })

  it('shows nothing-available copy when the card has no legal moves', async () => {
    stubFetch({ 'GET /api/v1/cards/ATLAS-1/transitions': () => jsonResponse([]) })
    renderWithClient(<TransitionButtons cardKey="ATLAS-1" />)
    expect(await screen.findByText('No moves available from here.')).toBeInTheDocument()
  })
})
