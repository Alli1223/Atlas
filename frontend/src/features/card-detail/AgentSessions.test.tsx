import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import { AgentSessions } from './AgentSessions'
import { makeAgentSession, makeCard, renderWithClient } from './test-support'

afterEach(() => {
  vi.unstubAllGlobals()
})

const CARD = makeCard({ key: 'ATLAS-1', summary: 'Add login' })

describe('AgentSessions', () => {
  it('shows an empty state and a working Run button when there is no history', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/cards/ATLAS-1/agent-sessions': () => jsonResponse([]),
      'POST /api/v1/cards/ATLAS-1/agent-sessions': () => jsonResponse(makeAgentSession(), 201),
    })

    renderWithClient(<AgentSessions card={CARD} />)

    expect(await screen.findByText('No runs yet.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Run with Claude' }))
    await waitFor(() => expect(calls).toContain('POST /api/v1/cards/ATLAS-1/agent-sessions'))
    expect(await screen.findByText('Running')).toBeInTheDocument()
  })

  it('lists past sessions with their status, cost and turns', async () => {
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/agent-sessions': () =>
        jsonResponse([
          makeAgentSession({
            id: 's-1',
            status: 'completed',
            resultText: 'Fixed it.',
            totalCostUsd: 0.42,
            numTurns: 3,
          }),
        ]),
    })

    renderWithClient(<AgentSessions card={CARD} />)

    expect(await screen.findByText('Completed')).toBeInTheDocument()
    expect(screen.getByText('$0.42 · 3 turns')).toBeInTheDocument()
    expect(screen.getByText('Fixed it.')).toBeInTheDocument()
  })

  it('shows a failed session with its error message', async () => {
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/agent-sessions': () =>
        jsonResponse([
          makeAgentSession({
            id: 's-2',
            status: 'failed',
            errorMessage: 'Invalid API key',
          }),
        ]),
    })

    renderWithClient(<AgentSessions card={CARD} />)

    expect(await screen.findByText('Failed')).toBeInTheDocument()
    expect(screen.getByText('Invalid API key')).toBeInTheDocument()
  })

  it('disables the Run button while the newest session is still running', async () => {
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/agent-sessions': () =>
        jsonResponse([makeAgentSession({ id: 's-3', status: 'running' })]),
    })

    renderWithClient(<AgentSessions card={CARD} />)

    await screen.findByText('Running')
    expect(screen.getByRole('button', { name: 'Run with Claude' })).toBeDisabled()
  })

  it('surfaces a start failure instead of silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/cards/ATLAS-1/agent-sessions': () => jsonResponse([]),
      'POST /api/v1/cards/ATLAS-1/agent-sessions': () =>
        problemResponse(
          'urn:atlas:error:conflict',
          409,
          "this project's repo link has no credential",
        ),
    })

    renderWithClient(<AgentSessions card={CARD} />)

    await screen.findByText('No runs yet.')
    await user.click(screen.getByRole('button', { name: 'Run with Claude' }))
    expect(
      await screen.findByText("this project's repo link has no credential"),
    ).toBeInTheDocument()
  })
})
