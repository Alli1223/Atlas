import { Link } from '@tanstack/react-router'
import {
  Bell,
  ChevronLeft,
  FolderKanban,
  LayoutDashboard,
  type LucideIcon,
  Palette,
  Plus,
  Settings,
  Star,
} from 'lucide-react'
import { type ReactNode } from 'react'

import { Avatar, Button, Input } from '@/components/ui'
import { ICON } from '@/lib/icon'
import { useUI } from '@/stores/ui'

import styles from './AppShell.module.css'
import { ThemeToggle } from './ThemeToggle'

/** `to` omitted = the destination is not built yet; render an inert row rather than a
 *  link to '/', which would light up as the current page and lie to the user. */
interface NavItem {
  label: string
  icon: LucideIcon
  to?: '/' | '/styleguide'
}

const NAV_SECTIONS: { heading: string; items: NavItem[] }[] = [
  {
    heading: 'Work',
    items: [
      { label: 'Overview', to: '/', icon: LayoutDashboard },
      { label: 'Boards', icon: FolderKanban },
      { label: 'Starred', icon: Star },
    ],
  },
  {
    heading: 'Atlas',
    items: [
      { label: 'Style guide', to: '/styleguide', icon: Palette },
      { label: 'Settings', icon: Settings },
    ],
  },
]

function TopNav() {
  return (
    <header className={styles.topnav}>
      <Link to="/" className={styles.brand}>
        <img src="/atlas.svg" alt="" className={styles.brandMark} />
        Atlas
      </Link>

      <Button appearance="primary" size="compact" iconBefore={<Plus {...ICON} aria-hidden="true" />}>
        Create
      </Button>

      <div className={styles.search}>
        {/* Disabled placeholder: real search is AQL-backed and lands with the query language. */}
        <Input type="search" size="compact" placeholder="Search" aria-label="Search Atlas" disabled />
      </div>

      <div className={styles.topnavSpacer} />

      <div className={styles.topnavActions}>
        <ThemeToggle />
        <Button
          appearance="subtle"
          isIconOnly
          aria-label="Notifications"
          iconBefore={<Bell {...ICON} aria-hidden="true" />}
        />
        <Avatar name="Alastair Rayner" size="small" />
      </div>
    </header>
  )
}

function SideNavItem({ item }: { item: NavItem }) {
  const contents = (
    <>
      <span className={styles.itemIcon}>
        <item.icon {...ICON} aria-hidden="true" />
      </span>
      {item.label}
    </>
  )

  if (item.to === undefined) {
    return (
      <span className={styles.item} aria-disabled="true" title="Not built yet">
        {contents}
      </span>
    )
  }

  return (
    <Link to={item.to} className={styles.item} activeOptions={{ exact: true }}>
      {contents}
    </Link>
  )
}

function SideNav() {
  const isCollapsed = useUI((state) => state.isSidebarCollapsed)

  return (
    <nav
      className={styles.sidenav}
      data-collapsed={isCollapsed}
      aria-label="Main"
      // Collapsed, the strip has no reachable content — inert keeps it out of the tab
      // order. The toggle lives outside it and stays reachable.
      inert={isCollapsed}
    >
      <div className={styles.sidenavContents}>
        {NAV_SECTIONS.map((section) => (
          <div key={section.heading} className={styles.sidenavSection}>
            <span className={styles.sidenavHeading}>{section.heading}</span>
            {section.items.map((item) => (
              <SideNavItem key={item.label} item={item} />
            ))}
          </div>
        ))}
      </div>
    </nav>
  )
}

function SideNavToggle() {
  const isCollapsed = useUI((state) => state.isSidebarCollapsed)
  const toggleSidebar = useUI((state) => state.toggleSidebar)

  return (
    <button
      type="button"
      className={styles.sidenavToggle}
      data-collapsed={isCollapsed}
      onClick={toggleSidebar}
      aria-label={isCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
      aria-expanded={!isCollapsed}
    >
      <ChevronLeft {...ICON} aria-hidden="true" />
    </button>
  )
}

export interface AppShellProps {
  children: ReactNode
}

export function AppShell({ children }: AppShellProps) {
  return (
    <div className={styles.shell}>
      <TopNav />
      <div className={styles.body}>
        <div className={styles.sidenavWrap}>
          <SideNav />
          <SideNavToggle />
        </div>
        <main className={styles.content}>{children}</main>
      </div>
    </div>
  )
}
