import { type ReactNode, useState } from 'react'

import { Button } from '@/components/ui'
import kit from '@/styles/kit.module.css'

import styles from './charts.module.css'

/** One named series of values, one per x position. */
export interface ChartSeries {
  id: string
  name: string
  values: number[]
}

const W = 640
const H = 240
const PAD = { top: 12, right: 12, bottom: 24, left: 40 }
const SLOTS = 8

/** Slot colour by the series' position in its (fixed, server-given) order — never by rank. */
export const seriesColour = (index: number) => `var(--series-${Math.min(index, SLOTS - 1) + 1})`

/**
 * At most eight series: past seven, the rest fold into one "Other" — a ninth hue is never
 * generated (a job search has nine statuses; the last two share "Other").
 */
export function foldSeries(series: ChartSeries[]): ChartSeries[] {
  if (series.length <= SLOTS) return series
  const kept = series.slice(0, SLOTS - 1)
  const rest = series.slice(SLOTS - 1)
  const length = Math.max(...rest.map((s) => s.values.length))
  return [
    ...kept,
    {
      id: 'other',
      name: `Other (${rest.map((s) => s.name).join(', ')})`,
      values: Array.from({ length }, (_, i) => rest.reduce((sum, s) => sum + (s.values[i] ?? 0), 0)),
    },
  ]
}

const isEmpty = (series: ChartSeries[]) => series.every((s) => s.values.every((v) => v === 0))

function ticks(max: number): number[] {
  if (max <= 0) return [0]
  const step = Math.pow(10, Math.floor(Math.log10(max)))
  const nice = [1, 2, 5, 10].map((m) => m * step).find((s) => max / s <= 4) ?? step * 10
  const out: number[] = []
  for (let v = 0; v <= max + nice * 0.001; v += nice) out.push(Number(v.toFixed(6)))
  return out
}

interface FrameProps {
  title: string
  labels: string[]
  series: ChartSeries[]
  unit?: string
  children: (scale: { x: (i: number) => number; y: (v: number) => number; max: number }) => ReactNode
  /** The plotted maximum (a stack's total, not one series'). */
  max: number
  /** Bars hover by band; lines and areas by nearest point. */
  band?: boolean
}

/**
 * The frame every chart shares: axes with a recessive grid, the legend (always, for ≥ 2
 * series), a crosshair + tooltip on hover, and a table view for anyone who cannot or would
 * rather not read the plot.
 */
export function ChartFrame({ title, labels, series, unit, children, max, band = false }: FrameProps) {
  const [hover, setHover] = useState<number | undefined>()
  const [asTable, setAsTable] = useState(false)
  const n = Math.max(labels.length, 1)
  const innerW = W - PAD.left - PAD.right
  const innerH = H - PAD.top - PAD.bottom
  const yTicks = ticks(max)
  const top = Math.max(yTicks.at(-1) ?? 1, 1)
  const x = band
    ? (i: number) => PAD.left + (innerW / n) * (i + 0.5)
    : (i: number) => PAD.left + (n === 1 ? innerW / 2 : (innerW / (n - 1)) * i)
  const y = (v: number) => PAD.top + innerH - (v / top) * innerH
  const every = Math.ceil(n / 8)

  return (
    <section className={`${kit.section} ${styles.viz}`} aria-label={title}>
      <div className={kit.header}>
        <h3 className={kit.subtitle}>{title}</h3>
        <span className={kit.spacer} />
        <Button size="compact" appearance="subtle" onClick={() => setAsTable((v) => !v)}>
          {asTable ? 'Show chart' : 'Show table'}
        </Button>
      </div>
      {series.length > 1 && (
        <div className={kit.legend}>
          {series.map((s, i) => (
            <span key={s.id}>
              <span className={kit.swatch} style={{ background: seriesColour(i) }} />
              {s.name}
            </span>
          ))}
        </div>
      )}
      {isEmpty(series) && !asTable ? (
        <p className={kit.muted}>
          Nothing to plot yet. Reports read a daily snapshot of the board; the first is taken within
          the hour, and history builds from there.
        </p>
      ) : asTable ? (
        <div style={{ overflowX: 'auto' }}>
          <table className={kit.table}>
            <thead>
              <tr>
                <th />
                {series.map((s) => (
                  <th key={s.id}>{s.name}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {labels.map((label, i) => (
                <tr key={label + String(i)}>
                  <td>{label}</td>
                  {series.map((s) => (
                    <td key={s.id}>{s.values[i] ?? ''}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div style={{ position: 'relative' }}>
          <svg
            className={kit.chart}
            viewBox={`0 0 ${W} ${H}`}
            role="img"
            aria-label={`${title}. Use "Show table" for the values.`}
            onMouseLeave={() => setHover(undefined)}
            onMouseMove={(event) => {
              const box = event.currentTarget.getBoundingClientRect()
              const px = ((event.clientX - box.left) / box.width) * W
              const step = band ? innerW / n : innerW / Math.max(n - 1, 1)
              const raw = band ? (px - PAD.left) / step - 0.5 : (px - PAD.left) / step
              setHover(Math.max(0, Math.min(n - 1, Math.round(raw))))
            }}
          >
            {yTicks.map((t) => (
              <g key={t}>
                <line className={styles.grid} x1={PAD.left} x2={W - PAD.right} y1={y(t)} y2={y(t)} />
                <text className={styles.axis} x={PAD.left - 6} y={y(t) + 3} textAnchor="end">
                  {t}
                </text>
              </g>
            ))}
            {labels.map((label, i) =>
              i % every === 0 ? (
                <text key={label + String(i)} className={styles.axis} x={x(i)} y={H - 6} textAnchor="middle">
                  {label.length > 10 ? label.slice(5) : label}
                </text>
              ) : null,
            )}
            {children({ x, y, max: top })}
            {hover !== undefined && (
              <line className={styles.crosshair} x1={x(hover)} x2={x(hover)} y1={PAD.top} y2={PAD.top + innerH} />
            )}
          </svg>
          {hover !== undefined && (
            <div
              className={styles.tooltip}
              style={{ left: `${(x(hover) / W) * 100}%`, transform: hover > n / 2 ? 'translateX(-105%)' : 'translateX(5%)' }}
            >
              <strong>{labels[hover]}</strong>
              {series.map((s, i) => (
                <div key={s.id}>
                  <span className={kit.swatch} style={{ background: seriesColour(i) }} />
                  {s.name}: {s.values[hover] ?? 0}
                  {unit ? ` ${unit}` : ''}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  )
}

/** Lines, 2px, one per series. */
export function LineChart({ title, labels, series: raw, unit }: { title: string; labels: string[]; series: ChartSeries[]; unit?: string }) {
  const series = foldSeries(raw)
  const max = Math.max(0, ...series.flatMap((s) => s.values))
  // Sparse data gets markers: a lone point has no line to show it.
  const markers = labels.length <= 40
  return (
    <ChartFrame title={title} labels={labels} series={series} max={max} {...(unit ? { unit } : {})}>
      {({ x, y }) =>
        series.map((s, i) => (
          <g key={s.id}>
            <polyline
              fill="none"
              stroke={seriesColour(i)}
              strokeWidth={2}
              strokeLinejoin="round"
              points={s.values.map((v, j) => `${x(j)},${y(v)}`).join(' ')}
            />
            {markers &&
              s.values.map((v, j) => (
                <circle key={String(j)} cx={x(j)} cy={y(v)} r={4} fill={seriesColour(i)} stroke="var(--ds-surface)" strokeWidth={2} />
              ))}
          </g>
        ))
      }
    </ChartFrame>
  )
}

/** Stacked areas, first series at the bottom, with a thin surface seam between bands. */
export function StackedAreaChart({ title, labels, series: raw }: { title: string; labels: string[]; series: ChartSeries[] }) {
  const series = foldSeries(raw)
  const totals = labels.map((_, j) => series.reduce((sum, s) => sum + (s.values[j] ?? 0), 0))
  const max = Math.max(0, ...totals)
  return (
    <ChartFrame title={title} labels={labels} series={series} max={max}>
      {({ x, y }) => {
        const base = labels.map(() => 0)
        return series.map((s, i) => {
          const lower = [...base]
          s.values.forEach((v, j) => {
            base[j] = (base[j] ?? 0) + v
          })
          const upper = [...base]
          const path =
            upper.map((v, j) => `${j === 0 ? 'M' : 'L'}${x(j)},${y(v)}`).join(' ') +
            lower
              .map((v, j) => [j, v] as const)
              .reverse()
              .map(([j, v]) => `L${x(j)},${y(v)}`)
              .join(' ') +
            'Z'
          return <path key={s.id} d={path} fill={seriesColour(i)} stroke="var(--ds-surface)" strokeWidth={1} />
        })
      }}
    </ChartFrame>
  )
}

/** Grouped bars with a 2px gap and rounded data-ends. */
export function BarChart({ title, labels, series }: { title: string; labels: string[]; series: ChartSeries[] }) {
  const max = Math.max(0, ...series.flatMap((s) => s.values))
  const n = Math.max(labels.length, 1)
  const bandW = (W - PAD.left - PAD.right) / n
  const barW = Math.max(4, Math.min(28, (bandW * 0.7) / Math.max(series.length, 1) - 2))
  return (
    <ChartFrame title={title} labels={labels} series={series} max={max} band>
      {({ x, y }) =>
        labels.flatMap((_, j) =>
          series.map((s, i) => {
            const v = s.values[j] ?? 0
            const left = x(j) - (series.length * (barW + 2)) / 2 + i * (barW + 2)
            const h = Math.max(0, y(0) - y(v))
            return <rect key={`${s.id}-${String(j)}`} x={left} y={y(v)} width={barW} height={h} rx={Math.min(4, h / 2)} fill={seriesColour(i)} />
          }),
        )
      }
    </ChartFrame>
  )
}
