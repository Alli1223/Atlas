import { Link } from '@tanstack/react-router'

import { Avatar } from '@/components/ui'

import type { Project } from './api'
import styles from './ProjectGrid.module.css'

/** Turns a template id into a short human label for the card's meta line. */
const TEMPLATE_LABEL: Record<string, string> = {
  programming: 'Software',
  modeling: '3D modeling',
  'job-search': 'Job search',
  blank: 'Blank',
}

function templateLabel(template: string): string {
  return TEMPLATE_LABEL[template] ?? template.replace(/-/g, ' ')
}

export interface ProjectCardProps {
  project: Project
}

/** One project tile: avatar, key, name, and what seeded it. The whole tile is the link. */
export function ProjectCard({ project }: ProjectCardProps) {
  return (
    <Link
      to="/projects/$projectKey/board"
      params={{ projectKey: project.key }}
      className={styles.card}
      aria-label={`Open the ${project.name} board`}
    >
      <Avatar
        name={project.name}
        {...(project.avatarUrl != null ? { src: project.avatarUrl } : {})}
        appearance="square"
        size="large"
      />
      <div className={styles.body}>
        <span className={styles.key}>{project.key}</span>
        <span className={styles.name}>{project.name}</span>
        <span className={styles.meta}>{templateLabel(project.template)}</span>
      </div>
    </Link>
  )
}

export interface ProjectGridProps {
  projects: Project[]
}

/** The grid of project tiles on the projects landing route. */
export function ProjectGrid({ projects }: ProjectGridProps) {
  return (
    <div className={styles.grid}>
      {projects.map((project) => (
        <ProjectCard key={project.id} project={project} />
      ))}
    </div>
  )
}
