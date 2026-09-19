import { useState } from 'react'

import { EmptyState, Spinner } from '@/components/ui'

import type { BurndownPoint } from './api'
import styles from './BurndownChart.module.css'
import { useBurndown } from './queries'

const WIDTH = 640
const HEIGHT = 220
const PADDING = { top: 16, right: 16, bottom: 28, left: 40 }

/** A day-of-month label, e.g. `taken_at` `"2026-01-14T00:00:00Z"` → `"14 Jan"`. */
function dayLabel(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

/**
 * A cycle's burndown: two lines over the snapshotted days — the committed scope (dashed, a
 * reference rather than a second data series) and what remains (solid). One axis, no dual
 * scale: both lines share the same unit, whichever [`BurndownMetric`] the response says it
 * is — cards counted, or their estimate summed, decided server-side by the project's
 * estimation setting, never guessed here.
 */
export function BurndownChart({ cycleId, enabled }: { cycleId: string; enabled: boolean }) {
  const burndown = useBurndown(cycleId, enabled)

  if (!enabled) return null

  if (burndown.isPending) {
    return (
      <div className={styles.state}>
        <Spinner size="small" label="Loading burndown" />
      </div>
    )
  }

  if (burndown.isError || !burndown.data) {
    return (
      <div className={styles.state}>
        <p className={styles.fieldMuted}>Could not load the burndown.</p>
      </div>
    )
  }

  const { points, metric } = burndown.data
  const unitLabel = metric === 'count' ? 'cards' : 'estimate'

  if (points.length === 0) {
    return (
      <div className={styles.state}>
        <EmptyState
          header="No burndown yet"
          description="The daily snapshot job hasn't reached this cycle yet — check back after the next run."
        />
      </div>
    )
  }

  return (
    <div className={styles.chart}>
      <BurndownSvg points={points} unitLabel={unitLabel} />
      <div className={styles.legend}>
        <span className={styles.legendItem}>
          <svg width="16" height="2" aria-hidden="true">
            <line x1="0" y1="1" x2="16" y2="1" stroke="var(--ds-icon-brand)" strokeWidth="2" />
          </svg>
          Remaining
        </span>
        <span className={styles.legendItem}>
          <svg width="16" height="2" aria-hidden="true">
            <line
              x1="0"
              y1="1"
              x2="16"
              y2="1"
              stroke="var(--ds-border-bold)"
              strokeWidth="2"
              strokeDasharray="3 3"
            />
          </svg>
          Total scope
        </span>
      </div>
    </div>
  )
}

function BurndownSvg({
  points,
  unitLabel,
}: {
  points: BurndownPoint[]
  unitLabel: string
}) {
  const [hoverIndex, setHoverIndex] = useState<number | null>(null)

  const innerWidth = WIDTH - PADDING.left - PADDING.right
  const innerHeight = HEIGHT - PADDING.top - PADDING.bottom
  const maxValue = Math.max(...points.map((p) => p.total), 1)

  const x = (index: number) =>
    points.length === 1 ? PADDING.left : PADDING.left + (index / (points.length - 1)) * innerWidth
  const y = (value: number) => PADDING.top + innerHeight - (value / maxValue) * innerHeight

  const remainingPath = points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${x(i)} ${y(p.remaining)}`).join(' ')
  const totalPath = points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${x(i)} ${y(p.total)}`).join(' ')

  const hovered = hoverIndex != null ? points[hoverIndex] : undefined

  return (
    <div className={styles.svgWrap}>
      <svg
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        role="img"
        aria-label={`Burndown chart: ${unitLabel} remaining over ${points.length} day${points.length === 1 ? '' : 's'}`}
      >
        {/* Recessive gridlines at 0%, 50%, 100% of scope. */}
        {[0, 0.5, 1].map((fraction) => (
          <line
            key={fraction}
            x1={PADDING.left}
            x2={WIDTH - PADDING.right}
            y1={PADDING.top + innerHeight * (1 - fraction)}
            y2={PADDING.top + innerHeight * (1 - fraction)}
            className={styles.gridline}
          />
        ))}

        <text x={PADDING.left - 8} y={PADDING.top + 4} textAnchor="end" className={styles.axisLabel}>
          {Math.round(maxValue)}
        </text>
        <text x={PADDING.left - 8} y={PADDING.top + innerHeight} textAnchor="end" className={styles.axisLabel}>
          0
        </text>
        <text x={PADDING.left} y={HEIGHT - 8} textAnchor="start" className={styles.axisLabel}>
          {dayLabel(points[0]!.date)}
        </text>
        <text
          x={WIDTH - PADDING.right}
          y={HEIGHT - 8}
          textAnchor="end"
          className={styles.axisLabel}
        >
          {dayLabel(points[points.length - 1]!.date)}
        </text>

        <path d={totalPath} className={styles.totalLine} fill="none" />
        <path d={remainingPath} className={styles.remainingLine} fill="none" />

        {points.map((point, index) => (
          <g key={point.date}>
            <circle cx={x(index)} cy={y(point.remaining)} r={3} className={styles.remainingDot} />
            {/* A larger, invisible hit target — the visible dot alone is well under the
                44px touch minimum, and this is what the hover tooltip actually listens on. */}
            <circle
              cx={x(index)}
              cy={y(point.remaining)}
              r={10}
              className={styles.hitTarget}
              onMouseEnter={() => setHoverIndex(index)}
              onMouseLeave={() => setHoverIndex(null)}
            />
          </g>
        ))}
      </svg>

      {hovered && (
        <div
          className={styles.tooltip}
          style={{ left: `${(x(hoverIndex!) / WIDTH) * 100}%` }}
        >
          <strong>{dayLabel(hovered.date)}</strong>
          <span>
            {hovered.remaining} of {hovered.total} {unitLabel} remaining
          </span>
        </div>
      )}
    </div>
  )
}
