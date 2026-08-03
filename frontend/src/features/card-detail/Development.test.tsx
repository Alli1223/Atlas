import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { ADMIN, jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import { Development } from './Development'
import { makeActivity, makeCard, makeGitLink, makeRepo, renderWithClient } from './test-support'

afterEach(() => {
  vi.unstubAllGlobals()
})

const CARD = makeCard({ key: 'ATLAS-1', summary: 'Add login' })
const NOT_FOUND = 'urn:atlas:error:not-found'

describe('Development', () => {
  it('offers an admin a way to link a repo when none is linked', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => problemResponse(NOT_FOUND, 404),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('No repository linked.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Link a repo' })).toBeInTheDocument()
  })

  it('hides the link and unlink actions from a non-admin', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse({ ...ADMIN, role: 'member' }),
      'GET /api/v1/projects/ATLAS/repo': () => problemResponse(NOT_FOUND, 404),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('No repository linked.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Link a repo' })).not.toBeInTheDocument()
  })

  it('shows the linked repo and creates a branch on click', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
      'POST /api/v1/cards/ATLAS-1/branch': () =>
        jsonResponse({
          branch: 'feature/ATLAS-1-add-login',
          url: 'https://github.com/octocat/hello/tree/feature/ATLAS-1-add-login',
          baseBranch: 'main',
        }),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('octocat/hello')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Create branch' }))
    await waitFor(() => expect(calls).toContain('POST /api/v1/cards/ATLAS-1/branch'))
  })

  it("lists the card's git links", async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () =>
        jsonResponse([makeGitLink({ reference: 'feature/ATLAS-1-add-login' })]),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    const link = await screen.findByText('feature/ATLAS-1-add-login')
    expect(link).toHaveAttribute(
      'href',
      'https://github.com/octocat/hello/tree/feature/ATLAS-1-add-login',
    )
  })

  it('surfaces a create-branch failure instead of silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
      'POST /api/v1/cards/ATLAS-1/branch': () =>
        problemResponse('urn:atlas:error:internal', 500, 'GitHub is unreachable'),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    await screen.findByText('octocat/hello')
    await user.click(screen.getByRole('button', { name: 'Create branch' }))
    expect(await screen.findByText('GitHub is unreachable')).toBeInTheDocument()
  })

  it('offers no Create PR action before a branch exists', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    await screen.findByText('octocat/hello')
    expect(screen.queryByRole('button', { name: 'Create PR' })).not.toBeInTheDocument()
  })

  it('offers Create PR once a branch exists, and creates one on click', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      'POST /api/v1/cards/ATLAS-1/pr': () =>
        jsonResponse(
          makeGitLink({ kind: 'pr', reference: '9', url: 'https://github.com/octocat/hello/pull/9' }),
        ),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    const prButton = await screen.findByRole('button', { name: 'Create PR' })
    await user.click(prButton)
    await waitFor(() => expect(calls).toContain('POST /api/v1/cards/ATLAS-1/pr'))
  })

  it('hides Create PR once a PR is already recorded', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () =>
        jsonResponse([makeGitLink(), makeGitLink({ kind: 'pr', reference: '9' })]),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    await screen.findByText('octocat/hello')
    expect(screen.queryByRole('button', { name: 'Create PR' })).not.toBeInTheDocument()
  })

  it('surfaces a create-PR failure instead of silently doing nothing', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      'POST /api/v1/cards/ATLAS-1/pr': () =>
        problemResponse('urn:atlas:error:conflict', 409, 'the linked repo has no usable credential'),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    const prButton = await screen.findByRole('button', { name: 'Create PR' })
    await user.click(prButton)
    expect(
      await screen.findByText('the linked repo has no usable credential'),
    ).toBeInTheDocument()
  })

  it('shows a CI badge and the commit list once a branch has activity', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      'GET /api/v1/cards/ATLAS-1/activity': () =>
        jsonResponse(
          makeActivity({
            ciStatus: 'failed',
            commits: [
              {
                sha: 'aaa1111',
                message: 'Add the login form',
                htmlUrl: 'https://github.com/octocat/hello/commit/aaa1111',
              },
            ],
          }),
        ),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('Checks failed')).toBeInTheDocument()
    const commit = await screen.findByText('Add the login form')
    expect(commit).toHaveAttribute('href', 'https://github.com/octocat/hello/commit/aaa1111')
  })

  it('never asks for activity before a branch exists', async () => {
    const { calls } = stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([]),
      'GET /api/v1/cards/ATLAS-1/activity': () => jsonResponse(makeActivity()),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    await screen.findByText('octocat/hello')
    expect(screen.queryByText(/^Checks /)).not.toBeInTheDocument()
    expect(calls).not.toContain('GET /api/v1/cards/ATLAS-1/activity')
  })

  it('shows the review rollup alongside the CI badge', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      'GET /api/v1/cards/ATLAS-1/activity': () =>
        jsonResponse(makeActivity({ reviewState: 'changesrequested' })),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('Changes requested')).toBeInTheDocument()
  })

  it('warns of merge conflicts only when mergeable is explicitly false', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      'GET /api/v1/cards/ATLAS-1/activity': () => jsonResponse(makeActivity({ mergeable: false })),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    expect(await screen.findByText('This PR has merge conflicts.')).toBeInTheDocument()
  })

  it('never shows a conflict warning while mergeable is still unknown', async () => {
    stubFetch({
      'GET /api/v1/auth/me': () => jsonResponse(ADMIN),
      'GET /api/v1/projects/ATLAS/repo': () => jsonResponse(makeRepo()),
      'GET /api/v1/cards/ATLAS-1/git-links': () => jsonResponse([makeGitLink()]),
      // mergeable is absent — GitHub has not finished computing it yet.
      'GET /api/v1/cards/ATLAS-1/activity': () => jsonResponse(makeActivity({ ciStatus: 'passed' })),
    })

    renderWithClient(<Development card={CARD} projectKey="ATLAS" />)

    await screen.findByText('Checks passed')
    expect(screen.queryByText(/merge conflicts/i)).not.toBeInTheDocument()
  })
})
