import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'
import { renderWithClient } from '@/features/card-detail/test-support'

import { ProjectSettingsPage } from './ProjectSettingsPage'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('ProjectSettingsPage', () => {
  it('loads the destination and persists a changed category', async () => {
    const user = userEvent.setup()
    const { calls } = stubFetch({
      'GET /api/v1/projects/ATLAS': () =>
        jsonResponse({ key: 'ATLAS', agentCompletionCategory: 'in_progress' }),
      'PATCH /api/v1/projects/ATLAS': () =>
        jsonResponse({ key: 'ATLAS', agentCompletionCategory: 'done' }),
    })

    renderWithClient(<ProjectSettingsPage projectKey="ATLAS" />)

    const select = await screen.findByRole('combobox', { name: 'Move finished cards to' })
    expect(select).toHaveValue('in_progress')
    await user.selectOptions(select, 'done')
    await waitFor(() => expect(calls).toContain('PATCH /api/v1/projects/ATLAS'))
    await waitFor(() => expect(select).toHaveValue('done'))
    const request = vi.mocked(fetch).mock.calls
      .map(([input]) => input)
      .find((input): input is Request => input instanceof Request && input.method === 'PATCH')
    expect(request).toBeDefined()
    expect(await request?.clone().json()).toEqual({ agentCompletionCategory: 'done' })
  })

  it('keeps the saved category and explains a rejected update', async () => {
    const user = userEvent.setup()
    stubFetch({
      'GET /api/v1/projects/ATLAS': () =>
        jsonResponse({ key: 'ATLAS', agentCompletionCategory: 'in_progress' }),
      'PATCH /api/v1/projects/ATLAS': () =>
        problemResponse('urn:atlas:error:forbidden', 403, 'Only project owners can edit settings.'),
    })

    renderWithClient(<ProjectSettingsPage projectKey="ATLAS" />)

    const select = await screen.findByRole('combobox', { name: 'Move finished cards to' })
    await user.selectOptions(select, 'done')
    expect(await screen.findByText('Only project owners can edit settings.')).toBeInTheDocument()
    expect(select).toHaveValue('in_progress')
  })

  it('shows a load error instead of an empty page', async () => {
    stubFetch({
      'GET /api/v1/projects/ATLAS': () =>
        problemResponse('urn:atlas:error:not-found', 404, 'Project not found.'),
    })

    renderWithClient(<ProjectSettingsPage projectKey="ATLAS" />)

    expect(await screen.findByText('Could not load project settings.')).toBeInTheDocument()
  })
})
