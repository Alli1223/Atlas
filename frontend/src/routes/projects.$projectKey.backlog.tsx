import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { z } from 'zod'

import { Banner, Spinner } from '@/components/ui'
import { ApiError } from '@/lib/api'
import {
  BacklogView,
  backlogQueryOptions,
  useBacklog,
  useBoardReferences,
} from '@/features/board'
import { CardDetailModal } from '@/features/card-detail'
import { useProject } from '@/features/projects'

import styles from './projects.$projectKey.backlog.module.css'

const searchSchema = z.object({
  /** The open card's key — read by the card-detail modal. Kept here so it survives reload. */
  card: z.string().optional(),
})

export const Route = createFileRoute('/projects/$projectKey/backlog')({
  validateSearch: searchSchema,
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(backlogQueryOptions(params.projectKey)),
  component: BacklogRoute,
})

function BacklogRoute() {
  const { projectKey } = Route.useParams()
  const search = Route.useSearch()
  const navigate = useNavigate({ from: Route.fullPath })

  const project = useProject(projectKey)
  const backlog = useBacklog(projectKey)
  const references = useBoardReferences(projectKey)

  const openCard = (cardKey: string) => {
    void navigate({ search: (prev) => ({ ...prev, card: cardKey }) })
  }
  const closeCard = () => {
    void navigate({ search: (prev) => ({ ...prev, card: undefined }) })
  }
  // The backlog has no nested view of its own — opening a board-bearing card's board is the
  // ordinary board route, scoped to that card.
  const openBoard = (cardKey: string) => {
    void navigate({
      to: '/projects/$projectKey/board',
      params: { projectKey },
      search: { parent: cardKey, trail: [], swimlane: 'none', filters: [] },
    })
  }

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div className={styles.headingRow}>
          <Link
            to="/projects/$projectKey/board"
            params={{ projectKey }}
            className={styles.backLink}
          >
            ← {project.data?.name ?? projectKey}
          </Link>
          <h1 className={styles.title}>Backlog</h1>
        </div>
      </header>

      {backlog.isError ? (
        <div className={styles.state}>
          <Banner appearance="error">
            {backlog.error instanceof ApiError
              ? (backlog.error.problem?.detail ?? 'Could not load the backlog.')
              : 'Could not load the backlog.'}
          </Banner>
        </div>
      ) : backlog.isPending || !backlog.data ? (
        <div className={styles.state}>
          <Spinner size="large" />
        </div>
      ) : (
        <BacklogView
          projectKey={projectKey}
          backlog={backlog.data}
          references={references}
          onOpen={openCard}
          onOpenBoard={openBoard}
        />
      )}

      {search.card !== undefined && (
        <CardDetailModal cardKey={search.card} onClose={closeCard} />
      )}
    </div>
  )
}
