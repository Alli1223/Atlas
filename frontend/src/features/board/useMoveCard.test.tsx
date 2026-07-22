import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { jsonResponse, problemResponse } from '@/features/auth/test-support'

import type { BoardCard, BoardColumn, BoardData } from './api'
import { boardKeys } from './queries'
import { type CardMove, useMoveCard } from './queries'
import { useToasts } from './toast'

function card(id: string, statusId: string): BoardCard {
  return {
    id,
    key: id.toUpperCase(),
    summary: id,
    typeId: 't',
    parentId: null,
    statusId,
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    rank: id,
    estimate: null,
    tags: [],
    childRollup: null,
  }
}

function column(statusId: string, name: string, ids: string[]): BoardColumn {
  return { status: { id: statusId, name, category: 'todo' }, cards: ids.map((i) => card(i, statusId)) }
}

/** To Do [a] · Doing []. */
function seedBoard(): BoardData {
  return { columns: [column('todo', 'To Do', ['a']), column('doing', 'Doing', [])] }
}

const PROJECT = 'ATLAS'
const PARAMS = {}
const KEY = boardKeys.data(PROJECT, PARAMS)

/** Where card `a` currently lives in a board snapshot. */
function columnOf(board: BoardData | undefined, cardId: string): string | undefined {
  return board?.columns.find((col) => col.cards.some((c) => c.id === cardId))?.status.id
}

let queryClient: QueryClient

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
}

/** A cross-column move of card `a` from To Do to Doing. */
const CROSS_COLUMN: CardMove = {
  card: card('a', 'todo'),
  toStatusId: 'doing',
  toIndex: 0,
  sameColumn: false,
  fromStatusName: 'To Do',
  toStatusName: 'Doing',
}

beforeEach(() => {
  queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  queryClient.setQueryData(KEY, seedBoard())
  useToasts.setState({ toasts: [] })
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('useMoveCard — optimistic move and rollback', () => {
  it('moves the card in the cache immediately, before the transition resolves, then keeps it on success', async () => {
    // A deferred so the transition-execute request stays pending while we assert optimism.
    let resolveExec: (r: Response) => void = () => undefined
    const execPending = new Promise<Response>((resolve) => {
      resolveExec = resolve
    })
    const posts: string[] = []

    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = input instanceof Request ? input.url : String(input)
        const method = (input instanceof Request ? input.method : init?.method) ?? 'GET'
        const path = url.replace(/^https?:\/\/[^/]+/, '')
        if (method === 'GET' && path === '/api/v1/cards/A/transitions') {
          return Promise.resolve(
            jsonResponse([{ id: 't1', name: 'Start', toStatusId: 'doing' }]),
          )
        }
        if (method === 'POST' && path === '/api/v1/cards/A/transitions/t1') {
          posts.push(path)
          return execPending
        }
        return Promise.reject(new Error(`unstubbed ${method} ${path}`))
      }),
    )

    const { result } = renderHook(() => useMoveCard(PROJECT, PARAMS), { wrapper })

    act(() => {
      result.current.mutate(CROSS_COLUMN)
    })

    // The card is in Doing before the server has answered — the "feels instant" guarantee.
    await waitFor(() => {
      expect(columnOf(queryClient.getQueryData<BoardData>(KEY), 'a')).toBe('doing')
    })
    expect(posts).toEqual(['/api/v1/cards/A/transitions/t1'])

    // The server accepts the transition → the card stays put.
    act(() => {
      resolveExec(jsonResponse({ id: 'a', key: 'A', statusId: 'doing' }))
    })
    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(columnOf(queryClient.getQueryData<BoardData>(KEY), 'a')).toBe('doing')
  })

  it('rolls the card back and toasts when the transition is rejected', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = input instanceof Request ? input.url : String(input)
        const method = (input instanceof Request ? input.method : init?.method) ?? 'GET'
        const path = url.replace(/^https?:\/\/[^/]+/, '')
        if (method === 'GET' && path === '/api/v1/cards/A/transitions') {
          return Promise.resolve(jsonResponse([{ id: 't1', name: 'Start', toStatusId: 'doing' }]))
        }
        if (method === 'POST' && path === '/api/v1/cards/A/transitions/t1') {
          return Promise.resolve(
            problemResponse('urn:atlas:error:validation', 422, 'A required field is missing.'),
          )
        }
        return Promise.reject(new Error(`unstubbed ${method} ${path}`))
      }),
    )

    const { result } = renderHook(() => useMoveCard(PROJECT, PARAMS), { wrapper })

    act(() => {
      result.current.mutate(CROSS_COLUMN)
    })

    await waitFor(() => expect(result.current.isError).toBe(true))
    // The card is back where it started — the optimistic move did not stick.
    expect(columnOf(queryClient.getQueryData<BoardData>(KEY), 'a')).toBe('todo')
    // ...and the failure was surfaced, in the server's own words, not swallowed.
    const toasts = useToasts.getState().toasts
    expect(toasts).toHaveLength(1)
    expect(toasts[0]).toMatchObject({ appearance: 'error', message: 'A required field is missing.' })
  })

  it('rejects an illegal drop with no matching transition, without calling the server, and rolls back', async () => {
    const posts: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = input instanceof Request ? input.url : String(input)
        const method = (input instanceof Request ? input.method : init?.method) ?? 'GET'
        const path = url.replace(/^https?:\/\/[^/]+/, '')
        if (method === 'GET' && path === '/api/v1/cards/A/transitions') {
          // The only legal move goes to In Review, never to the target Doing column.
          return Promise.resolve(jsonResponse([{ id: 't9', name: 'Review', toStatusId: 'review' }]))
        }
        posts.push(`${method} ${path}`)
        return Promise.reject(new Error(`unexpected ${method} ${path}`))
      }),
    )

    const { result } = renderHook(() => useMoveCard(PROJECT, PARAMS), { wrapper })

    act(() => {
      result.current.mutate(CROSS_COLUMN)
    })

    await waitFor(() => expect(result.current.isError).toBe(true))
    // No transition-execute and no raw move were attempted — the drop was refused client-side.
    expect(posts).toEqual([])
    expect(columnOf(queryClient.getQueryData<BoardData>(KEY), 'a')).toBe('todo')
    expect(useToasts.getState().toasts[0]?.message).toMatch(/can.t move to Doing/i)
  })

  it('takes the rank-aware move endpoint (not a transition) under a permissive workflow', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = input instanceof Request ? input.url : String(input)
        const method = (input instanceof Request ? input.method : init?.method) ?? 'GET'
        const path = url.replace(/^https?:\/\/[^/]+/, '')
        if (method === 'GET' && path === '/api/v1/cards/A/transitions') {
          // A permissive move: legal, but carries no transition id.
          return Promise.resolve(jsonResponse([{ id: null, name: 'Move to Doing', toStatusId: 'doing' }]))
        }
        calls.push(`${method} ${path}`)
        if (method === 'POST' && path === '/api/v1/cards/A/move') {
          return Promise.resolve(jsonResponse({ id: 'a', key: 'A', statusId: 'doing' }))
        }
        return Promise.reject(new Error(`unstubbed ${method} ${path}`))
      }),
    )

    const { result } = renderHook(() => useMoveCard(PROJECT, PARAMS), { wrapper })

    act(() => {
      result.current.mutate(CROSS_COLUMN)
    })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))
    expect(calls).toEqual(['POST /api/v1/cards/A/move'])
    expect(columnOf(queryClient.getQueryData<BoardData>(KEY), 'a')).toBe('doing')
  })
})
