import { screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { jsonResponse, stubFetch } from '@/features/auth/test-support'
import { renderWithClient } from '@/features/card-detail/test-support'
import { ProviderUsage } from './ProviderUsage'
afterEach(() => vi.unstubAllGlobals())
it('shows recorded usage without inventing cost for subscription providers', async () => {
  stubFetch({ 'GET /api/v1/agent-usage': () => jsonResponse([
    {provider:'codex',runs:2,activeRuns:1,measuredRuns:2,inputTokens:12000,outputTokens:400,cachedTokens:8000,costUsd:null},
    {provider:'claude',runs:1,activeRuns:0,measuredRuns:1,inputTokens:20,outputTokens:100,cachedTokens:500,costUsd:0.125},
  ]) })
  renderWithClient(<ProviderUsage />)
  expect(await screen.findByText('1 active · 3 calls')).toBeInTheDocument()
  expect(screen.getByText('12,000')).toBeInTheDocument()
  expect(screen.getByText('$0.1250')).toBeInTheDocument()
  expect(screen.getAllByText('Not reported').length).toBeGreaterThan(0)
  expect(screen.getByText(/not your subscription allowance/)).toBeInTheDocument()
})
