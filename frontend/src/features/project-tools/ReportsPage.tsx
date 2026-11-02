import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'

import { Banner, Select, Spinner } from '@/components/ui'
import { type ApiError, api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'
import kit from '@/styles/kit.module.css'

import { BarChart, LineChart, StackedAreaChart } from './charts'

type DailyChart = components['schemas']['DailyChart']

const RANGES = [
  { value: '14', label: 'Last 14 days' },
  { value: '30', label: 'Last 30 days' },
  { value: '90', label: 'Last 90 days' },
]

const dayLabels = (chart: DailyChart) => chart.days

function Daily({
  title,
  query,
  kind,
}: {
  title: string
  query: { isPending: boolean; error: unknown; data: DailyChart | undefined }
  kind: 'area' | 'line'
}) {
  if (query.isPending) return <Spinner />
  if (query.error) return <Banner appearance="warning">{(query.error as ApiError).message}</Banner>
  const chart = query.data
  if (!chart) return null
  const series = chart.series.map((s) => ({ id: s.id, name: s.name, values: s.values }))
  return kind === 'area' ? (
    <StackedAreaChart title={title} labels={dayLabels(chart)} series={series} />
  ) : (
    <LineChart title={title} labels={dayLabels(chart)} series={series} unit={chart.unit} />
  )
}

/**
 * A project's reports (`TODO.md` Phase 16), every one a query over the daily snapshot:
 * cumulative flow, created vs resolved, average age, the control chart and — when cycles
 * are on — velocity.
 */
export function ReportsPage({ projectKey }: { projectKey: string }) {
  const [days, setDays] = useState('30')
  const path = { params: { path: { key: projectKey }, query: { days: Number(days) } } }
  const cfd = useQuery({
    queryKey: ['reports', projectKey, 'cfd', days],
    queryFn: async () => unwrap(await api.GET('/api/v1/projects/{key}/reports/cumulative-flow', path)),
  })
  const created = useQuery({
    queryKey: ['reports', projectKey, 'created', days],
    queryFn: async () => unwrap(await api.GET('/api/v1/projects/{key}/reports/created-vs-resolved', path)),
  })
  const age = useQuery({
    queryKey: ['reports', projectKey, 'age', days],
    queryFn: async () => unwrap(await api.GET('/api/v1/projects/{key}/reports/average-age', path)),
  })
  const control = useQuery({
    queryKey: ['reports', projectKey, 'control', days],
    queryFn: async () => unwrap(await api.GET('/api/v1/projects/{key}/reports/control-chart', path)),
  })
  const velocity = useQuery({
    queryKey: ['reports', projectKey, 'velocity'],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/projects/{key}/reports/velocity', { params: { path: { key: projectKey } } })),
    retry: false,
  })

  return (
    <div className={kit.page}>
      <div className={kit.header}>
        <h1 className={kit.title}>Reports</h1>
        <span className={kit.spacer} />
        <Select aria-label="Time range" options={RANGES} value={days} onChange={(e) => setDays(e.target.value)} />
      </div>
      <div className={kit.grid}>
        <Daily title="Cumulative flow" query={cfd} kind="area" />
        <Daily title="Created vs resolved" query={created} kind="line" />
        <Daily title="Average age (days)" query={age} kind="line" />
        {control.data && control.data.points.length > 0 ? (
          <LineChart
            title={`Cycle time — mean ${control.data.mean.toFixed(1)} d, median ${control.data.median.toFixed(1)} d`}
            labels={control.data.points.map((p) => p.key)}
            unit="days"
            series={[
              { id: 'cycle', name: 'Cycle time', values: control.data.points.map((p) => p.cycleDays) },
              { id: 'rolling', name: 'Rolling average', values: control.data.points.map((p) => p.rollingAverage) },
            ]}
          />
        ) : (
          <section className={kit.section}>
            <h3 className={kit.subtitle}>Cycle time</h3>
            <p className={kit.muted}>Nothing resolved in this range yet.</p>
          </section>
        )}
        {velocity.data && velocity.data.cycles.length > 0 && (
          <BarChart
            title={`Velocity — average ${velocity.data.average.toFixed(1)} ${velocity.data.unit}`}
            labels={velocity.data.cycles.map((c) => c.name)}
            series={[
              { id: 'committed', name: 'Committed', values: velocity.data.cycles.map((c) => c.committed) },
              { id: 'completed', name: 'Completed', values: velocity.data.cycles.map((c) => c.completed) },
            ]}
          />
        )}
      </div>
    </div>
  )
}
