import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, expect, it, vi } from 'vitest'
import { jsonResponse, stubFetch } from '@/features/auth/test-support'
import { renderWithClient } from '@/features/card-detail/test-support'
import { AutoBoard } from './AutoBoard'
import { CardAutomation } from './CardAutomation'

afterEach(() => vi.unstubAllGlobals())
it('starts a coordinator with the selected provider and batch size', async () => {
  const user = userEvent.setup()
  const { calls } = stubFetch({
    'GET /api/v1/projects/ATLAS/auto': () => jsonResponse(null),
    'POST /api/v1/projects/ATLAS/auto': () => jsonResponse({id:'job',status:'running',maxTasks:3,completedTasks:0,message:'Choosing a ticket',console:''}, 201),
  })
  renderWithClient(<AutoBoard projectKey="ATLAS" />)
  await user.click(screen.getByText('Auto', {selector:'summary'}))
  await user.selectOptions(screen.getByRole('combobox', {name:'Coordinator'}), 'claude')
  await user.click(screen.getByRole('button', {name:/^Auto$/}))
  expect(calls).toContain('POST /api/v1/projects/ATLAS/auto')
  const req = vi.mocked(fetch).mock.calls.map(([input]) => input).find((input): input is Request => input instanceof Request && input.method === 'POST')
  expect(await req?.clone().json()).toEqual({provider:'claude',maxTasks:3})
})
it('shows the current task and stops dispatching new work', async () => {
  const user = userEvent.setup()
  stubFetch({
    'GET /api/v1/projects/ATLAS/auto': () => jsonResponse({id:'job',status:'running',maxTasks:3,completedTasks:0,currentCardKey:'ATLAS-4',message:'Codex is working',console:''}),
    'POST /api/v1/projects/ATLAS/auto/stop': () => new Response(null, {status:204}),
  })
  renderWithClient(<AutoBoard projectKey="ATLAS" />)
  await user.click(await screen.findByText(/Auto · running/, {selector:'summary'}))
  expect(screen.getByRole('link', {name:'Open ATLAS-4'})).toHaveAttribute('href','/cards/ATLAS-4')
  await user.click(screen.getByRole('button', {name:'Stop Auto'}))
  expect(vi.mocked(fetch).mock.calls.some(([input]) => input instanceof Request && input.url.endsWith('/auto/stop'))).toBe(true)
})
it('shows agent assignments and links to unfinished blocking tickets', async () => {
  stubFetch({
    'GET /api/v1/cards/ATLAS-2/automation': () => jsonResponse({assignment:{provider:'codex',reason:'Best for tests'},blockedBy:[{key:'ATLAS-1',summary:'Foundation',reason:'Needs schema',done:false}]}),
  })
  renderWithClient(<CardAutomation cardKey="ATLAS-2" />)
  expect(await screen.findByText('Assigned agent: codex')).toBeInTheDocument()
  expect(screen.getByRole('link', {name:'ATLAS-1'})).toHaveAttribute('href','/cards/ATLAS-1')
  expect(screen.getByText('Needs schema')).toBeInTheDocument()
})
