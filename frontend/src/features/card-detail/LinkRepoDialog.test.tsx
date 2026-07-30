import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import { LinkRepoDialog } from './LinkRepoDialog'
import { makeCredential, makeRepo, renderWithClient } from './test-support'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('LinkRepoDialog', () => {
  it('links the entered repo with the (defaulted) credential and closes', async () => {
    const user = userEvent.setup()
    const onClose = vi.fn()
    const { calls } = stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([makeCredential({ id: 'cred-1' })]),
      'PUT /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={onClose} />)

    // Wait for the credential list to load so the selection defaults to it.
    await screen.findByRole('option', { name: 'work laptop' })
    await user.type(screen.getByLabelText(/^Owner/), 'octocat')
    await user.type(screen.getByLabelText(/^Repository/), 'hello')
    await user.click(screen.getByRole('button', { name: 'Link repository' }))

    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(calls).toContain('PUT /api/v1/projects/ATLAS/repo')
  })

  it('surfaces a link failure and stays open', async () => {
    const user = userEvent.setup()
    const onClose = vi.fn()
    stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([makeCredential({ id: 'cred-1' })]),
      'PUT /api/v1/projects/ATLAS/repo': () =>
        problemResponse('urn:atlas:error:internal', 500, 'GitHub said no'),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={onClose} />)

    await screen.findByRole('option', { name: 'work laptop' })
    await user.type(screen.getByLabelText(/^Owner/), 'octocat')
    await user.type(screen.getByLabelText(/^Repository/), 'hello')
    await user.click(screen.getByRole('button', { name: 'Link repository' }))

    expect(await screen.findByText('GitHub said no')).toBeInTheDocument()
    expect(onClose).not.toHaveBeenCalled()
  })

  it('prompts to add a credential when there are none', async () => {
    stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([]),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={vi.fn()} />)

    expect(await screen.findByText(/No GitHub credential is stored yet/i)).toBeInTheDocument()
  })
})
