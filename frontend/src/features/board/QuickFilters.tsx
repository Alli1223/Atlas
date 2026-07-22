import { cx } from '@/lib/cx'

import styles from './QuickFilters.module.css'

/**
 * A quick filter is a labelled AQL fragment. Toggling several ANDs them together onto the
 * board's scope — the board endpoint ANDs the whole thing onto its own `project = …` scope
 * and runs it through the same AQL compiler as `POST /search`, so a filter can never widen
 * what the board shows, only narrow it.
 *
 * This is deliberately ~a dozen lines of data: the entire quick-filter feature is a list of
 * `{ label, aql }` and a toggle, because AQL already does the work.
 */
export interface QuickFilter {
  id: string
  label: string
  aql: string
}

export const QUICK_FILTERS: QuickFilter[] = [
  { id: 'mine', label: 'My issues', aql: 'assignee = currentUser()' },
  { id: 'unassigned', label: 'Unassigned', aql: 'assignee IS EMPTY' },
  { id: 'bugs', label: 'Bugs', aql: 'type = Bug' },
  { id: 'high', label: 'High priority', aql: 'priority >= High' },
]

/** Combines the active quick filters into one AQL predicate, or `''` when none are active. */
export function combineFilters(active: ReadonlySet<string>): string {
  return QUICK_FILTERS.filter((filter) => active.has(filter.id))
    .map((filter) => `(${filter.aql})`)
    .join(' AND ')
}

export interface QuickFiltersProps {
  active: ReadonlySet<string>
  onToggle: (id: string) => void
}

/** The row of toggle chips. Multi-select; each pressed chip narrows the board further. */
export function QuickFilters({ active, onToggle }: QuickFiltersProps) {
  return (
    <div className={styles.filters} role="group" aria-label="Quick filters">
      {QUICK_FILTERS.map((filter) => {
        const isActive = active.has(filter.id)
        return (
          <button
            key={filter.id}
            type="button"
            className={cx(styles.chip, isActive && styles.active)}
            aria-pressed={isActive}
            onClick={() => onToggle(filter.id)}
          >
            {filter.label}
          </button>
        )
      })}
    </div>
  )
}
