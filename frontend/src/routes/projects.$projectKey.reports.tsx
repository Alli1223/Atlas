import { createFileRoute } from '@tanstack/react-router'

import { ReportsPage, ProjectTabs } from '@/features/project-tools'

export const Route = createFileRoute('/projects/$projectKey/reports')({
  component: function ReportsPageRoute() {
    const { projectKey } = Route.useParams()
    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
        <ProjectTabs projectKey={projectKey} />
        <ReportsPage projectKey={projectKey} />
      </div>
    )
  },
})
