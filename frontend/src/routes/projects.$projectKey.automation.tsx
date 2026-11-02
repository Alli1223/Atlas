import { createFileRoute } from '@tanstack/react-router'

import { AutomationPage, ProjectTabs } from '@/features/project-tools'

export const Route = createFileRoute('/projects/$projectKey/automation')({
  component: function AutomationPageRoute() {
    const { projectKey } = Route.useParams()
    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
        <ProjectTabs projectKey={projectKey} />
        <AutomationPage projectKey={projectKey} />
      </div>
    )
  },
})
