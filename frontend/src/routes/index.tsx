import { createFileRoute, Link } from '@tanstack/react-router'
import { Check } from 'lucide-react'

import { Button, Lozenge } from '@/components/ui'
import { ICON } from '@/lib/icon'

import styles from './index.module.css'

export const Route = createFileRoute('/')({
  component: OverviewRoute,
})

const DONE = [
  'Vite 8 (Rolldown) + React 19 + React Compiler 1.0',
  'Design tokens generated from the real ADS values',
  'Primitives: Button, Input, Lozenge, Tag, Avatar and friends',
  'Theme: light / dark / system, persisted, no flash on load',
]

function OverviewRoute() {
  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h1>Atlas</h1>
        <p className={styles.lede}>
          A self-hosted Jira equivalent where a card is the unit of work an agent picks up.
        </p>
      </header>

      <section className={styles.panel}>
        <h2 className={styles.panelTitle}>Foundation</h2>
        <div className={styles.statusRow}>
          <Lozenge statusCategory="done">Scaffold</Lozenge>
          <Lozenge statusCategory="inprogress">Design system</Lozenge>
          <Lozenge statusCategory="todo">Boards</Lozenge>
        </div>
        <ul className={styles.checklist}>
          {DONE.map((item) => (
            <li key={item} className={styles.checklistItem}>
              <Check {...ICON} aria-hidden="true" />
              {item}
            </li>
          ))}
        </ul>
        <div>
          <Link to="/styleguide">
            <Button appearance="primary">Open the style guide</Button>
          </Link>
        </div>
      </section>
    </div>
  )
}
