import { screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { ADMIN, jsonResponse, renderApp } from '@/features/auth/test-support'
import { useUI } from '@/stores/ui'

import type { BoardCard, BoardData, CardType, ChildRollup } from './api'

/**
 * Nested boards, end to end through the real route tree.
 *
 * This is the flagship "a card contains a board" journey: clicking a board-bearing card's
 * mini-map drills into its children (a `?parent=` scoped board), and the breadcrumb grows a
 * linkable segment per level. jsdom cannot drag, but this path is all navigation and query
 * state, which it *can* exercise faithfully — so the trail mechanism and the parent scoping
 * are proven here, not just asserted about.
 */

const PROJECT = 'ATLAS'
const TYPE: CardType = {
  id: 'type-story',
  projectId: 'p',
  name: 'Story',
  icon: 'bookmark',
  colour: '#1868DB',
  level: 0,
  isDefault: true,
}

function boardCard(key: string, summary: string, rollup: ChildRollup | null): BoardCard {
  return {
    id: `id-${key}`,
    key,
    summary,
    typeId: TYPE.id,
    parentId: null,
    statusId: 'st-todo',
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    rank: '8000',
    estimate: null,
    tags: [],
    childRollup: rollup,
  }
}

/** A one-column board holding the given cards. */
function board(cards: BoardCard[]): BoardData {
  return {
    columns: [{ status: { id: 'st-todo', name: 'To Do', category: 'todo' }, cards }],
  }
}

const ROLLUP: ChildRollup = { total: 5, todo: 2, inProgress: 1, done: 2 }

// The three nested levels: the project's root board holds "3D Modeling"; that card's board
// holds "Base Mesh"; and that card's board holds a leaf.
const ROOT_BOARD = board([boardCard(`${PROJECT}-1`, '3D Modeling', ROLLUP)])
const MODELLING_BOARD = board([boardCard(`${PROJECT}-2`, 'Base Mesh', ROLLUP)])
const MESH_BOARD = board([boardCard(`${PROJECT}-3`, 'Retopologise', null)])

function cardDto(key: string, summary: string) {
  return { id: `id-${key}`, key, projectId: 'p', typeId: TYPE.id, summary, statusId: 'st-todo', rank: '8000' }
}

/**
 * A fetch stub that dispatches on pathname (and, for the board, its `parent` query) rather
 * than an exact URL — so query-string ordering does not make the test brittle.
 */
function stubBoardApi() {
  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL) => {
      const url = new URL(input instanceof Request ? input.url : String(input), 'http://localhost')
      const { pathname } = url
      const parent = url.searchParams.get('parent')

      if (pathname === '/api/v1/auth/me') return Promise.resolve(jsonResponse(ADMIN))
      if (pathname === `/api/v1/projects/${PROJECT}`)
        return Promise.resolve(jsonResponse({ id: 'p', key: PROJECT, name: 'Programming' }))
      if (pathname === `/api/v1/projects/${PROJECT}/board`) {
        const data = parent === `${PROJECT}-1` ? MODELLING_BOARD : parent === `${PROJECT}-2` ? MESH_BOARD : ROOT_BOARD
        return Promise.resolve(jsonResponse(data))
      }
      if (pathname === `/api/v1/projects/${PROJECT}/card-types`)
        return Promise.resolve(jsonResponse([TYPE]))
      if (pathname === `/api/v1/projects/${PROJECT}/priorities`) return Promise.resolve(jsonResponse([]))
      if (pathname === '/api/v1/users') return Promise.resolve(jsonResponse([]))
      if (pathname === `/api/v1/projects/${PROJECT}/boards`) return Promise.resolve(jsonResponse([]))
      if (pathname === `/api/v1/cards/${PROJECT}-1`)
        return Promise.resolve(jsonResponse(cardDto(`${PROJECT}-1`, '3D Modeling')))
      if (pathname === `/api/v1/cards/${PROJECT}-2`)
        return Promise.resolve(jsonResponse(cardDto(`${PROJECT}-2`, 'Base Mesh')))

      return Promise.reject(new Error(`unstubbed request: ${pathname}`))
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
  useUI.setState({ theme: 'system', isSidebarCollapsed: false })
})

/** The breadcrumb nav — the one landmark that records the nesting path. */
function breadcrumb() {
  return screen.getByRole('navigation', { name: 'Breadcrumb' })
}

describe('nested boards', () => {
  it('drills into a card’s board and grows a linkable breadcrumb per level', async () => {
    stubBoardApi()
    const { router } = renderApp(`/projects/${PROJECT}/board`)

    // Level 0 — the project's root board. The project is the current crumb (not a link).
    const modelling = await screen.findByRole('button', { name: /Open ATLAS-1.*board/ })
    expect(within(breadcrumb()).queryByRole('link', { name: 'Programming' })).toBeNull()

    // Drill into "3D Modeling".
    await userEvent.click(modelling)

    // Level 1 — the URL is now scoped to ATLAS-1, and the breadcrumb reads
    // Projects › Programming › 3D Modeling, with Programming now a link back to the root.
    await within(breadcrumb()).findByText('3D Modeling')
    expect(router.state.location.search).toMatchObject({ parent: `${PROJECT}-1`, trail: [] })
    expect(within(breadcrumb()).getByRole('link', { name: 'Programming' })).toBeInTheDocument()
    // "3D Modeling" is the current level, so it is text, not a link.
    expect(within(breadcrumb()).queryByRole('link', { name: '3D Modeling' })).toBeNull()

    // Drill again into "Base Mesh".
    await userEvent.click(await screen.findByRole('button', { name: /Open ATLAS-2.*board/ }))

    // Level 2 — parent is ATLAS-2 and the trail records ATLAS-1 above it. The breadcrumb is
    // now Projects › Programming › 3D Modeling › Base Mesh, and the grandparent is a link.
    await within(breadcrumb()).findByText('Base Mesh')
    expect(router.state.location.search).toMatchObject({
      parent: `${PROJECT}-2`,
      trail: [`${PROJECT}-1`],
    })
    expect(within(breadcrumb()).getByRole('link', { name: 'Programming' })).toBeInTheDocument()
    expect(within(breadcrumb()).getByRole('link', { name: '3D Modeling' })).toBeInTheDocument()
    expect(within(breadcrumb()).queryByRole('link', { name: 'Base Mesh' })).toBeNull()
  })

  it('walks back up the tree when a breadcrumb link is followed', async () => {
    stubBoardApi()
    const { router } = renderApp(`/projects/${PROJECT}/board`)

    // Drill two levels deep, to ATLAS-2's board with ATLAS-1 in the trail.
    await userEvent.click(await screen.findByRole('button', { name: /Open ATLAS-1.*board/ }))
    await userEvent.click(await screen.findByRole('button', { name: /Open ATLAS-2.*board/ }))
    await within(breadcrumb()).findByText('Base Mesh')

    // Following the grandparent link returns to ATLAS-1's board and truncates the trail —
    // the ancestor is no longer above the current board, so it leaves the URL.
    await userEvent.click(within(breadcrumb()).getByRole('link', { name: '3D Modeling' }))

    await within(breadcrumb()).findByText('3D Modeling')
    expect(router.state.location.search).toMatchObject({ parent: `${PROJECT}-1`, trail: [] })
    expect(within(breadcrumb()).queryByRole('link', { name: '3D Modeling' })).toBeNull()

    // And up once more to the project root clears the parent entirely.
    await userEvent.click(within(breadcrumb()).getByRole('link', { name: 'Programming' }))
    await screen.findByRole('button', { name: /Open ATLAS-1.*board/ })
    expect(router.state.location.search.parent).toBeUndefined()
  })
})
