import { createFileRoute, Link } from '@tanstack/react-router'

import { CycleList, projectCyclesQueryOptions } from '@/features/cycles'
import { useProject } from '@/features/projects'

import styles from './projects.$projectKey.cycles.module.css'

export const Route = createFileRoute('/projects/$projectKey/cycles')({
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(projectCyclesQueryOptions(params.projectKey)),
  component: CyclesRoute,
})

function CyclesRoute() {
  const { projectKey } = Route.useParams()
  const project = useProject(projectKey)

  return (
    <div className={styles.page}>
      <Link
        to="/projects/$projectKey/board"
        params={{ projectKey }}
        className={styles.backLink}
      >
        ← {project.data?.name ?? projectKey}
      </Link>
      <CycleList projectKey={projectKey} />
    </div>
  )
}
