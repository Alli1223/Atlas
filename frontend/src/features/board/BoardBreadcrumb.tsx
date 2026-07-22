import { Link } from '@tanstack/react-router'
import { ChevronRight } from 'lucide-react'
import { Fragment } from 'react'

import { ICON } from '@/lib/icon'

import styles from './BoardBreadcrumb.module.css'

/** One nested level: the parent card whose children a nested board renders. */
export interface BreadcrumbLevel {
  /** The parent card's key — the `?parent=` scope of that level's board. */
  key: string
  /** The parent card's summary, or its key if the summary has not loaded. */
  label: string
}

export interface BoardBreadcrumbProps {
  projectKey: string
  /** The project's display name — the root board's crumb. */
  projectName: string
  /**
   * The chain of nested parents from the outermost down to the board currently shown, in
   * order. Empty on the top-level board. The last entry is the current board (not a link).
   */
  levels: BreadcrumbLevel[]
  /** Carried onto every crumb's link so grouping survives a hop up the tree. */
  swimlane: string
  /** Carried onto every crumb's link so active quick filters survive a hop up the tree. */
  filters: string[]
}

/**
 * The nested-board breadcrumb: `Projects › Project › Grandparent › Parent › Card`.
 *
 * Every segment except the current board is a link, and each is fully deep-linkable — a
 * level's link encodes both its own `parent` scope *and* the `trail` of keys above it, so
 * following it reconstructs the exact breadcrumb, and browser back/forward move through the
 * nesting. This is the whole "a card contains a board, shareably" claim made navigable.
 */
export function BoardBreadcrumb({
  projectKey,
  projectName,
  levels,
  swimlane,
  filters,
}: BoardBreadcrumbProps) {
  const lastIndex = levels.length - 1

  return (
    <nav className={styles.breadcrumb} aria-label="Breadcrumb">
      <Link to="/projects" className={styles.crumb}>
        Projects
      </Link>
      <ChevronRight {...ICON} aria-hidden="true" className={styles.crumbSep} />

      {/* The project's root board. It is the current crumb only when no card is nested into. */}
      {levels.length === 0 ? (
        <span className={styles.crumbCurrent}>{projectName}</span>
      ) : (
        <Link
          to="/projects/$projectKey/board"
          params={{ projectKey }}
          search={{ swimlane: swimlane as 'none' | 'assignee' | 'parent', filters }}
          className={styles.crumb}
        >
          {projectName}
        </Link>
      )}

      {levels.map((level, index) => {
        const isCurrent = index === lastIndex
        // A level's own trail is every ancestor key above it — never including itself.
        const trail = levels.slice(0, index).map((l) => l.key)
        return (
          <Fragment key={level.key}>
            <ChevronRight {...ICON} aria-hidden="true" className={styles.crumbSep} />
            {isCurrent ? (
              <span className={styles.crumbCurrent}>{level.label}</span>
            ) : (
              <Link
                to="/projects/$projectKey/board"
                params={{ projectKey }}
                search={{
                  parent: level.key,
                  trail,
                  swimlane: swimlane as 'none' | 'assignee' | 'parent',
                  filters,
                }}
                className={styles.crumb}
              >
                {level.label}
              </Link>
            )}
          </Fragment>
        )
      })}
    </nav>
  )
}
