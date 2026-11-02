import { createFileRoute } from '@tanstack/react-router'

import { ImportExportPage, ProjectTabs } from '@/features/project-tools'

export const Route = createFileRoute('/projects/$projectKey/import')({
  component: function ImportExportPageRoute() {
    const { projectKey } = Route.useParams()
    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
        <ProjectTabs projectKey={projectKey} />
        <ImportExportPage projectKey={projectKey} />
      </div>
    )
  },
})
