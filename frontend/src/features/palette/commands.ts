/** A palette entry: a label, optional hint, and where it goes. */
export interface Command {
  id: string
  label: string
  hint?: string
  /** A route to navigate to, with its params and search. */
  to: string
  params?: Record<string, string>
  search?: Record<string, string>
}

export interface ProjectRef {
  key: string
  name: string
}

const CARD_KEY = /^[A-Za-z][A-Za-z0-9]*-\d+$/

/**
 * Every command for a query, best first: an exact card key opens that card; then the fixed
 * destinations and each project's pages, filtered by substring; then "search for …".
 */
export function commandsFor(query: string, projects: readonly ProjectRef[]): Command[] {
  const q = query.trim()
  const lower = q.toLowerCase()
  const all: Command[] = [
    { id: 'home', label: 'Go to overview', to: '/' },
    { id: 'projects', label: 'Go to projects', to: '/projects', hint: 'g p' },
    { id: 'search', label: 'Search cards (AQL)', to: '/search', hint: '/' },
    { id: 'integrations', label: 'Integrations and keys', to: '/settings' },
    { id: 'admin', label: 'Admin settings and backups', to: '/admin/settings' },
  ]
  for (const project of projects) {
    const params = { projectKey: project.key }
    all.push(
      { id: `${project.key}-board`, label: `${project.name}: board`, hint: project.key, to: '/projects/$projectKey/board', params },
      { id: `${project.key}-reports`, label: `${project.name}: reports`, hint: project.key, to: '/projects/$projectKey/reports', params },
      { id: `${project.key}-automation`, label: `${project.name}: automation`, hint: project.key, to: '/projects/$projectKey/automation', params },
      { id: `${project.key}-import`, label: `${project.name}: import / export`, hint: project.key, to: '/projects/$projectKey/import', params },
      { id: `${project.key}-trash`, label: `${project.name}: trash`, hint: project.key, to: '/projects/$projectKey/trash', params },
    )
  }
  const matches = q
    ? all.filter((c) => c.label.toLowerCase().includes(lower) || c.hint?.toLowerCase() === lower)
    : all
  const out: Command[] = []
  if (CARD_KEY.test(q)) {
    out.push({ id: 'open-card', label: `Open ${q.toUpperCase()}`, to: '/cards/$key', params: { key: q.toUpperCase() } })
  }
  out.push(...matches)
  if (q) {
    out.push({
      id: 'search-text',
      label: `Search for "${q}"`,
      to: '/search',
      search: { q: `text ~ "${q.replace(/"/g, '\\"')}"` },
    })
  }
  return out
}
