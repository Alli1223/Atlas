import { createFileRoute } from '@tanstack/react-router'

import { TrashPage, ProjectTabs } from '@/features/project-tools'

export const Route = createFileRoute('/projects/$projectKey/trash')({
  component: function TrashPageRoute() {
    const { projectKey } = Route.useParams()
    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
        <ProjectTabs projectKey={projectKey} />
        <TrashPage projectKey={projectKey} />
      </div>
    )
  },
})
