import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import { LinkRepoDialog } from './LinkRepoDialog'
import { makeCredential, makeGithubRepo, makeRepo, renderWithClient } from './test-support'

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

  it('fills owner and repository when a repo is picked from the list', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([makeCredential({ id: 'cred-1' })]),
      'GET /api/v1/credentials/cred-1/repos?page=1': () =>
        jsonResponse([makeGithubRepo({ id: 42, fullName: 'octocat/hello' })]),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={vi.fn()} />)

    await screen.findByRole('option', { name: 'work laptop' })
    await screen.findByRole('option', { name: 'octocat/hello' })
    const picker = screen.getByLabelText('Pick a repository')
    await user.selectOptions(picker, 'octocat/hello')

    expect(screen.getByLabelText(/^Owner/)).toHaveValue('octocat')
    expect(screen.getByLabelText(/^Repository/)).toHaveValue('hello')
  })

  it('marks a private repo in the picker and never lists one the token cannot push to', async () => {
    stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([makeCredential({ id: 'cred-1' })]),
      'GET /api/v1/credentials/cred-1/repos?page=1': () =>
        jsonResponse([
          makeGithubRepo({ id: 1, fullName: 'octocat/secret', private: true }),
          makeGithubRepo({ id: 2, fullName: 'octocat/read-only', canPush: false }),
        ]),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={vi.fn()} />)

    await screen.findByRole('option', { name: 'work laptop' })
    expect(
      await screen.findByRole('option', { name: 'octocat/secret (private)' }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('option', { name: /read-only/ }),
    ).not.toBeInTheDocument()
  })

  it('hides the picker rather than showing an empty one when the credential has no repos', async () => {
    stubFetch({
      'GET /api/v1/credentials': () => jsonResponse([makeCredential({ id: 'cred-1' })]),
      'GET /api/v1/credentials/cred-1/repos?page=1': () => jsonResponse([]),
    })

    renderWithClient(<LinkRepoDialog projectKey="ATLAS" onClose={vi.fn()} />)

    await screen.findByRole('option', { name: 'work laptop' })
    await waitFor(() =>
      expect(screen.queryByLabelText('Pick a repository')).not.toBeInTheDocument(),
    )
    // Manual entry is unaffected.
    expect(screen.getByLabelText(/^Owner/)).toBeInTheDocument()
  })
})
