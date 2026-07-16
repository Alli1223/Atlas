import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router'
import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'

import { ThemeProvider } from '@/providers/ThemeProvider'
import { createAppRouter } from '@/router'
import { useUI } from '@/stores/ui'

/**
 * Integration smoke tests. The unit tests cover each primitive in isolation; these mount
 * the real route tree, root route, shell and pages through the same factory main.tsx uses
 * — which is the only place a broken provider chain or a bad route definition shows up.
 */
function renderApp(initialPath = '/') {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [initialPath] }))

  return render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  )
}

// The zustand store is module-level and outlives a single test.
beforeEach(() => {
  useUI.setState({ theme: 'system', isSidebarCollapsed: false })
})

describe('app shell', () => {
  it('renders the overview route inside the shell', async () => {
    renderApp('/')

    expect(await screen.findByRole('heading', { name: 'Atlas', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'Main' })).toBeInTheDocument()
    expect(screen.getByRole('main')).toBeInTheDocument()
  })

  it('marks only the current page as current', async () => {
    renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    const nav = screen.getByRole('navigation', { name: 'Main' })
    const current = within(nav)
      .getAllByRole('link')
      .filter((link) => link.getAttribute('aria-current') === 'page')

    // Placeholder destinations render as inert rows rather than links to '/', precisely so
    // they cannot light up as the current page.
    expect(current).toHaveLength(1)
    expect(current[0]).toHaveTextContent('Overview')
  })

  it('navigates to the style guide', async () => {
    renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    await userEvent.click(screen.getByRole('link', { name: /Style guide/ }))

    expect(await screen.findByRole('heading', { name: 'Style guide' })).toBeInTheDocument()
  })

  it('collapses and expands the sidebar', async () => {
    renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    const toggle = screen.getByRole('button', { name: 'Collapse sidebar' })
    expect(toggle).toHaveAttribute('aria-expanded', 'true')

    await userEvent.click(toggle)

    expect(screen.getByRole('button', { name: 'Expand sidebar' })).toHaveAttribute(
      'aria-expanded',
      'false',
    )
    expect(useUI.getState().isSidebarCollapsed).toBe(true)

    await userEvent.click(screen.getByRole('button', { name: 'Expand sidebar' }))

    expect(screen.getByRole('button', { name: 'Collapse sidebar' })).toBeInTheDocument()
  })

  it('cycles the theme from the top nav', async () => {
    useUI.setState({ theme: 'light' })
    renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    await userEvent.click(screen.getByRole('button', { name: /Theme: Light/ }))

    expect(useUI.getState().theme).toBe('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
  })
})

describe('styleguide route', () => {
  it('renders every primitive in both themes without crashing', async () => {
    const { container } = renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Style guide' })).toBeInTheDocument()

    // Two theme islands, each a full showcase — this is what makes a token regression
    // visible side by side rather than reported later.
    //
    // Scope to the render container, not `document`: ThemeProvider puts data-theme on
    // <html> too, and a document-wide query would match that first and sweep up BOTH panes.
    const light = container.querySelector('[data-theme="light"]')
    const dark = container.querySelector('[data-theme="dark"]')
    expect(light).not.toBeNull()
    expect(dark).not.toBeNull()

    // Each pane shows the To Do lozenge twice — once subtle, once bold.
    expect(within(light as HTMLElement).getAllByText('To Do')).toHaveLength(2)
    expect(within(dark as HTMLElement).getAllByText('To Do')).toHaveLength(2)
    expect(screen.getAllByRole('heading', { name: 'Buttons' })).toHaveLength(2)
    expect(screen.getAllByRole('heading', { name: 'Lozenges' })).toHaveLength(2)
    expect(screen.getAllByRole('heading', { name: 'Board card' })).toHaveLength(2)
  })

  it('renders the theme islands independently of the active theme', async () => {
    useUI.setState({ theme: 'dark' })
    const { container } = renderApp('/styleguide')
    await screen.findByRole('heading', { name: 'Style guide' })

    // The light pane must stay light even while the app itself is dark, which only works
    // because the theme blocks are matched on any element rather than :root alone.
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(container.querySelector('[data-theme="light"]')).not.toBeNull()
  })
})
