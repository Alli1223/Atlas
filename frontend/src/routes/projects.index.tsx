import { createFileRoute } from '@tanstack/react-router'
import { FolderKanban, Plus } from 'lucide-react'
import { useState } from 'react'

import { Button, EmptyState, Skeleton } from '@/components/ui'
import {
  CreateProjectDialog,
  ProjectGrid,
  projectsQueryOptions,
  useProjects,
} from '@/features/projects'
import { ICON } from '@/lib/icon'

import styles from './projects.index.module.css'

export const Route = createFileRoute('/projects/')({
  loader: ({ context }) => context.queryClient.ensureQueryData(projectsQueryOptions()),
  component: ProjectsRoute,
})

function ProjectsRoute() {
  const { data: projects, isPending } = useProjects()
  const [isCreating, setIsCreating] = useState(false)

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div>
          <h1>Projects</h1>
          <p className={styles.lede}>Open a board, or start something new.</p>
        </div>
        <Button
          appearance="primary"
          onClick={() => setIsCreating(true)}
          iconBefore={<Plus {...ICON} aria-hidden="true" />}
        >
          Create project
        </Button>
      </header>

      {isPending ? (
        <div className={styles.skeletons}>
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} height="88px" className={styles.skeletonCard} />
          ))}
        </div>
      ) : projects && projects.length > 0 ? (
        <ProjectGrid projects={projects} />
      ) : (
        <EmptyState
          image={<FolderKanban size={48} strokeWidth={1.5} aria-hidden="true" />}
          header="No projects yet"
          description="A project holds a board of cards. Create your first one to get started."
          primaryAction={
            <Button appearance="primary" onClick={() => setIsCreating(true)}>
              Create project
            </Button>
          }
        />
      )}

      {isCreating && <CreateProjectDialog onClose={() => setIsCreating(false)} />}
    </div>
  )
}
