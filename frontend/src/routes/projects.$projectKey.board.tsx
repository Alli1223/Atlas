import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { z } from 'zod'

import { Banner, Select, Spinner } from '@/components/ui'
import { ApiError } from '@/lib/api'
import type { BoardParams, BreadcrumbLevel } from '@/features/board'
import {
  BoardBreadcrumb,
  BoardView,
  boardQueryOptions,
  combineFilters,
  QuickFilters,
  Toaster,
  useBoard,
  useBoardReferences,
  useCard,
  useCardSummaries,
  useSavedBoards,
} from '@/features/board'
import { CardDetailModal } from '@/features/card-detail'
import { useProject } from '@/features/projects'

import styles from './projects.$projectKey.board.module.css'

const searchSchema = z.object({
  /** A parent card key to render the children of — the nested board. */
  parent: z.string().optional(),
  /**
   * The ancestor card keys above `parent`, outermost first — the breadcrumb trail. Kept in
   * the URL so a nested board's full path is shareable and back/forward walk the nesting.
   * Excludes `parent` itself (that is the current board).
   */
  trail: z.array(z.string()).default([]),
  /** Row grouping. */
  swimlane: z.enum(['none', 'assignee', 'parent']).default('none'),
  /** Active quick-filter ids. */
  filters: z.array(z.string()).default([]),
  /** The open card's key — read by the card-detail modal. Kept here so it survives reload. */
  card: z.string().optional(),
})

export const Route = createFileRoute('/projects/$projectKey/board')({
  validateSearch: searchSchema,
  // Only the params that shape the fetch belong in the loader dependency — the open card
  // (`card`) does not change the board, so it must not re-run the loader.
  loaderDeps: ({ search }) => ({
    parent: search.parent,
    swimlane: search.swimlane,
    filters: search.filters,
  }),
  loader: ({ context, params, deps }) => {
    const boardParams: BoardParams = {
      swimlane: deps.swimlane,
      ...(deps.parent !== undefined && { parent: deps.parent }),
      ...(combineFilters(new Set(deps.filters)) !== '' && {
        aql: combineFilters(new Set(deps.filters)),
      }),
    }
    return context.queryClient.ensureQueryData(
      boardQueryOptions(params.projectKey, boardParams),
    )
  },
  component: BoardRoute,
})

const SWIMLANE_OPTIONS = [
  { label: 'No swimlanes', value: 'none' },
  { label: 'Group by assignee', value: 'assignee' },
  { label: 'Group by parent', value: 'parent' },
]

function BoardRoute() {
  const { projectKey } = Route.useParams()
  const search = Route.useSearch()
  const navigate = useNavigate({ from: Route.fullPath })

  const activeFilters = new Set(search.filters)
  const aql = combineFilters(activeFilters)
  const params: BoardParams = {
    swimlane: search.swimlane,
    ...(search.parent !== undefined && { parent: search.parent }),
    ...(aql !== '' && { aql }),
  }

  const project = useProject(projectKey)
  const board = useBoard(projectKey, params)
  const references = useBoardReferences(projectKey)
  const savedBoards = useSavedBoards(projectKey)
  const parentCard = useCard(search.parent)
  const trailSummaries = useCardSummaries(search.trail)

  const wipLimits: Record<string, number> = normaliseWipLimits(
    savedBoards.data?.[0]?.wipLimits,
  )

  // The nested-board path, outermost ancestor → current board. The trail keys carry summary
  // labels (fetched, falling back to the key); the current board is `parent`.
  const levels: BreadcrumbLevel[] = [
    ...search.trail.map((key) => ({ key, label: trailSummaries.get(key) ?? key })),
    ...(search.parent !== undefined
      ? [{ key: search.parent, label: parentCard.data?.summary ?? search.parent }]
      : []),
  ]

  const openCard = (cardKey: string) => {
    void navigate({ search: (prev) => ({ ...prev, card: cardKey }) })
  }
  // Closing the card is dropping the `?card=` param — the modal is URL-driven, so this both
  // dismisses it and makes back/forward walk in and out of a card.
  const closeCard = () => {
    void navigate({ search: (prev) => ({ ...prev, card: undefined }) })
  }
  // Drilling into a card's board pushes the *current* parent onto the trail, so the
  // breadcrumb grows one link deeper and stays a faithful, linkable record of the path.
  const openBoard = (cardKey: string) => {
    void navigate({
      search: (prev) => ({
        ...prev,
        parent: cardKey,
        trail: prev.parent !== undefined ? [...prev.trail, prev.parent] : [],
        card: undefined,
      }),
    })
  }
  const setSwimlane = (value: string) => {
    void navigate({ search: (prev) => ({ ...prev, swimlane: value as typeof search.swimlane }) })
  }
  const toggleFilter = (id: string) => {
    void navigate({
      search: (prev) => {
        const next = new Set(prev.filters)
        if (next.has(id)) next.delete(id)
        else next.add(id)
        return { ...prev, filters: [...next] }
      },
    })
  }

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div className={styles.headingRow}>
          <BoardBreadcrumb
            projectKey={projectKey}
            projectName={project.data?.name ?? projectKey}
            levels={levels}
            swimlane={search.swimlane}
            filters={search.filters}
          />
          <h1 className={styles.title}>
            {search.parent !== undefined
              ? (parentCard.data?.summary ?? 'Board')
              : (project.data?.name ?? projectKey)}
          </h1>
        </div>

        <div className={styles.controls}>
          <QuickFilters active={activeFilters} onToggle={toggleFilter} />
          <div className={styles.swimlaneControl}>
            <Select
              aria-label="Swimlanes"
              options={SWIMLANE_OPTIONS}
              value={search.swimlane}
              onChange={(event) => setSwimlane(event.target.value)}
            />
          </div>
        </div>
      </header>

      {board.isError ? (
        <div className={styles.state}>
          <Banner appearance="error">
            {board.error instanceof ApiError
              ? (board.error.problem?.detail ?? 'Could not load this board.')
              : 'Could not load this board.'}
          </Banner>
        </div>
      ) : board.isPending || !board.data ? (
        <div className={styles.state}>
          <Spinner size="large" />
        </div>
      ) : (
        <BoardView
          projectKey={projectKey}
          params={params}
          board={board.data}
          references={references}
          wipLimits={wipLimits}
          onOpen={openCard}
          onOpenBoard={openBoard}
        />
      )}

      {search.card !== undefined && (
        <CardDetailModal cardKey={search.card} onClose={closeCard} />
      )}

      <Toaster />
    </div>
  )
}

/** Coerces the saved board's `wip_limits` JSON object into a `{statusId: number}` map. */
function normaliseWipLimits(raw: unknown): Record<string, number> {
  if (raw === null || typeof raw !== 'object') return {}
  const out: Record<string, number> = {}
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value === 'number' && Number.isFinite(value)) out[key] = value
  }
  return out
}
