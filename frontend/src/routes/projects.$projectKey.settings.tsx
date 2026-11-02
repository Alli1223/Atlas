import { createFileRoute, Link } from '@tanstack/react-router'

import { projectQueryOptions, ProjectSettingsPage, useProject } from '@/features/projects'

import styles from './projects.$projectKey.settings.module.css'

export const Route = createFileRoute('/projects/$projectKey/settings')({
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(projectQueryOptions(params.projectKey)),
  component: SettingsRoute,
})

function SettingsRoute() {
  const { projectKey } = Route.useParams()
  const project = useProject(projectKey)

  return (
    <div className={styles.page}>
      <Link to="/projects/$projectKey/board" params={{ projectKey }} className={styles.backLink}>
        ← {project.data?.name ?? projectKey}
      </Link>
      <ProjectSettingsPage projectKey={projectKey} />
    </div>
  )
}
