import { Link, useNavigate, useRouterState } from '@tanstack/react-router'
import {
  ChevronLeft,
  FolderKanban,
  KeyRound,
  LayoutDashboard,
  type LucideIcon,
  Palette,
  Plus,
  Search,
  Settings,
} from 'lucide-react'
import { type ReactNode, useState } from 'react'

import { Avatar, Button, Input } from '@/components/ui'
import { useCurrentUser } from '@/features/auth/queries'
import { Toaster } from '@/features/board/Toaster'
import { NotificationBell, useLiveSync } from '@/features/notifications'
import { CommandPalette } from '@/features/palette'
import { ICON } from '@/lib/icon'
import { useUI } from '@/stores/ui'

import styles from './AppShell.module.css'
import { ErrorBoundary } from './ErrorBoundary'
import { OfflineBanner } from './OfflineBanner'
import { ThemeToggle } from './ThemeToggle'

/** `to` omitted = the destination is not built yet; render an inert row rather than a
 *  link to '/', which would light up as the current page and lie to the user. */
interface NavItem {
  label: string
  icon: LucideIcon
  to?: '/' | '/styleguide' | '/projects' | '/search' | '/settings' | '/admin/settings'
}

const NAV_SECTIONS: { heading: string; items: NavItem[] }[] = [
  {
    heading: 'Work',
    items: [
      { label: 'Overview', to: '/', icon: LayoutDashboard },
      { label: 'Projects', to: '/projects', icon: FolderKanban },
      { label: 'Search', to: '/search', icon: Search },
    ],
  },
  {
    heading: 'Atlas',
    items: [
      { label: 'Style guide', to: '/styleguide', icon: Palette },
      { label: 'Integrations', to: '/settings', icon: KeyRound },
      { label: 'Settings', to: '/admin/settings', icon: Settings },
    ],
  },
]

function TopNav() {
  const navigate = useNavigate()
  const { user } = useCurrentUser()
  const [query, setQuery] = useState('')
  return (
    <header className={styles.topnav}>
      <Link to="/" className={styles.brand}>
        <img src="/atlas.svg" alt="" className={styles.brandMark} />
        Atlas
      </Link>

      <Button
        appearance="primary"
        size="compact"
        iconBefore={<Plus {...ICON} aria-hidden="true" />}
        onClick={() => void navigate({ to: '/projects' })}
      >
        Create
      </Button>

      <form
        className={styles.search}
        role="search"
        onSubmit={(event) => {
          event.preventDefault()
          const text = query.trim()
          if (!text) return
          // A card key opens the card; anything else is AQL if it looks like it, text otherwise.
          if (/^[A-Za-z][A-Za-z0-9]*-\d+$/.test(text)) {
            void navigate({ to: '/cards/$key', params: { key: text.toUpperCase() } })
          } else {
            const aql = /[=~<>]| in /i.test(text) ? text : `text ~ "${text.replace(/"/g, '\\"')}"`
            void navigate({ to: '/search', search: { q: aql } })
          }
          setQuery('')
        }}
      >
        <Input
          type="search"
          size="compact"
          placeholder="Search or jump to a key  ( . for commands )"
          aria-label="Search Atlas"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </form>

      <div className={styles.topnavSpacer} />

      <div className={styles.topnavActions}>
        <ThemeToggle />
        <NotificationBell />
        <Avatar name={user?.displayName ?? user?.username ?? ''} size="small" />
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
  const { user } = useCurrentUser()
  const path = useRouterState({ select: (state) => state.location.pathname })
  useLiveSync(Boolean(user))
  return (
    <div className={styles.shell}>
      <TopNav />
      <div className={styles.body}>
        <div className={styles.sidenavWrap}>
          <SideNav />
          <SideNavToggle />
        </div>
        <main className={styles.content}>
          <ErrorBoundary resetKey={path}>{children}</ErrorBoundary>
        </main>
      </div>
      <CommandPalette />
      <Toaster />
      <OfflineBanner />
    </div>
  )
}
