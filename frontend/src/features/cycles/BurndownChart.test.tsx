import { render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, stubFetch } from '@/features/auth/test-support'

import type { Burndown } from './api'
import { BurndownChart } from './BurndownChart'

afterEach(() => {
  vi.unstubAllGlobals()
})

function renderChart(cycleId = 'cycle-1', enabled = true) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <BurndownChart cycleId={cycleId} enabled={enabled} />
    </QueryClientProvider>,
  )
}

describe('BurndownChart', () => {
  it('renders nothing at all when not enabled — no fetch, no DOM', () => {
    const { calls } = stubFetch({})
    const { container } = renderChart('cycle-1', false)
    expect(container).toBeEmptyDOMElement()
    expect(calls).toHaveLength(0)
  })

  it('shows an empty state when the snapshot job has not reached this cycle yet', async () => {
    stubFetch({
      'GET /api/v1/cycles/cycle-1/burndown': () =>
        jsonResponse({ metric: 'estimate', points: [] } satisfies Burndown),
    })

    renderChart()

    expect(await screen.findByText('No burndown yet')).toBeInTheDocument()
  })

  it('renders a legend and the accessible chart summary for a populated series', async () => {
    const burndown: Burndown = {
      metric: 'estimate',
      points: [
        { date: '2026-01-01T00:00:00Z', remaining: 8, total: 8 },
        { date: '2026-01-02T00:00:00Z', remaining: 5, total: 8 },
      ],
    }
    stubFetch({ 'GET /api/v1/cycles/cycle-1/burndown': () => jsonResponse(burndown) })

    renderChart()

    expect(
      await screen.findByRole('img', { name: 'Burndown chart: estimate remaining over 2 days' }),
    ).toBeInTheDocument()
    expect(screen.getByText('Remaining')).toBeInTheDocument()
    expect(screen.getByText('Total scope')).toBeInTheDocument()
  })

  it('labels the axis by count rather than estimate when the project has no estimation field', async () => {
    const burndown: Burndown = {
      metric: 'count',
      points: [{ date: '2026-01-01T00:00:00Z', remaining: 3, total: 3 }],
    }
    stubFetch({ 'GET /api/v1/cycles/cycle-1/burndown': () => jsonResponse(burndown) })

    renderChart()

    expect(
      await screen.findByRole('img', { name: 'Burndown chart: cards remaining over 1 day' }),
    ).toBeInTheDocument()
  })
})
