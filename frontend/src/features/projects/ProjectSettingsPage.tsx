import { Banner, Select } from '@/components/ui'

import type { StatusCategory } from './api'
import styles from './ProjectSettingsPage.module.css'
import { useProject, useUpdateProject } from './queries'

const CATEGORY_LABEL: Record<StatusCategory, string> = {
  todo: 'To Do (first status)',
  in_progress: 'In Progress (first status)',
  done: 'Done (first Done status)',
}

const CATEGORY_OPTIONS = (Object.keys(CATEGORY_LABEL) as StatusCategory[]).map((value) => ({
  label: CATEGORY_LABEL[value],
  value,
}))

/**
 * A project's own settings — currently just where a successful Claude Code run moves a
 * card. `TODO.md`'s own note flags "move it to the backlog" as an unusual destination for
 * *finished* work, so this is a per-project choice rather than a constant, defaulting to
 * In Review (`migrations/0017_agent_completion_category.sql`).
 */
export function ProjectSettingsPage({ projectKey }: { projectKey: string }) {
  const project = useProject(projectKey)
  const updateProject = useUpdateProject(projectKey)

  if (project.isError) {
    return <Banner appearance="error">Could not load project settings.</Banner>
  }

  if (project.isPending || !project.data) {
    return <p role="status">Loading project settings…</p>
  }

  return (
    <div className={styles.page}>
      <h1 className={styles.title}>Project settings</h1>

      <section className={styles.section} aria-labelledby="agent-completion-heading">
        <h2 id="agent-completion-heading" className={styles.sectionTitle}>
          Claude Code
        </h2>
        <p className={styles.sectionHint}>
          When a Claude Code run on a card finishes successfully, move the card here.
          The destination is the first status in the selected category.
        </p>

        {updateProject.isError && (
          <Banner appearance="error">
            {updateProject.error.problem?.detail ?? 'Could not update this setting.'}
          </Banner>
        )}

        <Select
          label="Move finished cards to"
          options={CATEGORY_OPTIONS}
          value={project.data.agentCompletionCategory}
          disabled={updateProject.isPending}
          onChange={(event) =>
            updateProject.mutate({
              agentCompletionCategory: event.target.value as StatusCategory,
            })
          }
        />
      </section>
    </div>
  )
}
