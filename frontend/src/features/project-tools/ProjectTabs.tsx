import { Link } from '@tanstack/react-router'

import kit from '@/styles/kit.module.css'

const TABS = [
  { to: '/projects/$projectKey/board', label: 'Board' },
  { to: '/projects/$projectKey/cycles', label: 'Cycles' },
  { to: '/projects/$projectKey/reports', label: 'Reports' },
  { to: '/projects/$projectKey/automation', label: 'Automation' },
  { to: '/projects/$projectKey/import', label: 'Import / export' },
  { to: '/projects/$projectKey/trash', label: 'Trash' },
] as const

/** The project's sections, as tabs. The router marks the active one `data-status="active"`. */
export function ProjectTabs({ projectKey }: { projectKey: string }) {
  return (
    <nav className={kit.tabs} aria-label="Project">
      {TABS.map((tab) => (
        <Link key={tab.to} to={tab.to} params={{ projectKey }} className={kit.tab}>
          {tab.label}
        </Link>
      ))}
    </nav>
  )
}
