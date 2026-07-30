import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render } from '@testing-library/react'
import { type ReactElement } from 'react'

import type { Card, CardGitLink, Credential, ProjectRepo } from './api'

/**
 * Test helpers for card-detail components that need a QueryClient but not the router.
 *
 * Not a `.test.tsx` file, so vitest does not run it as a suite. The three required suites
 * (inline-edit rollback, history, transitions) mount single components with a fresh cache
 * and a stubbed `fetch` — deliberately *not* the whole `/cards/$key` route, which would drag
 * in TipTap (a ProseMirror editor jsdom cannot host) for logic that has nothing to do with
 * the rich-text surface.
 */
export function renderWithClient(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const result = render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>)
  return { ...result, queryClient }
}

/** A minimal but complete card, overridable per test. */
export function makeCard(overrides: Partial<Card> = {}): Card {
  return {
    id: '019f-card',
    key: 'ATLAS-1',
    projectId: '019f-project',
    typeId: 'type-story',
    parentId: null,
    summary: 'Original summary',
    description: null,
    statusId: 'status-todo',
    priorityId: null,
    assigneeId: null,
    reporterId: null,
    creatorId: '019f-user',
    resolutionId: null,
    resolved: false,
    resolvedAt: null,
    dueDate: null,
    startDate: null,
    estimate: null,
    rank: '8000',
    archivedAt: null,
    createdAt: '2026-07-16T10:00:00Z',
    updatedAt: '2026-07-16T10:00:00Z',
    ...overrides,
  }
}

/** A project→repo link DTO, overridable per test. */
export function makeRepo(overrides: Partial<ProjectRepo> = {}): ProjectRepo {
  return {
    owner: 'octocat',
    repo: 'hello',
    fullName: 'octocat/hello',
    repoId: 42,
    defaultBranch: 'main',
    branchPrefix: 'feature',
    credentialId: 'cred-1',
    webhookConfigured: false,
    linkedAt: '2026-07-16T10:00:00Z',
    updatedAt: '2026-07-16T10:00:00Z',
    ...overrides,
  }
}

/** A card git-link DTO, overridable per test. */
export function makeGitLink(overrides: Partial<CardGitLink> = {}): CardGitLink {
  return {
    kind: 'branch',
    reference: 'feature/ATLAS-1-add-login',
    url: 'https://github.com/octocat/hello/tree/feature/ATLAS-1-add-login',
    state: null,
    createdAt: '2026-07-16T10:00:00Z',
    ...overrides,
  }
}

/** A GitHub credential DTO (metadata only), overridable per test. */
export function makeCredential(overrides: Partial<Credential> = {}): Credential {
  return {
    id: 'cred-1',
    provider: 'github',
    label: 'work laptop',
    lastFour: 'a1b2',
    status: 'valid',
    expiresAt: null,
    scopes: [],
    lastValidatedAt: null,
    createdAt: '2026-07-01T10:00:00Z',
    updatedAt: '2026-07-16T10:00:00Z',
    ...overrides,
  }
}
