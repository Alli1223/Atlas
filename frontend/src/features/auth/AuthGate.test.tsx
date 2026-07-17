import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { safeRedirect } from './AuthGate'
import { PROBLEM_TYPE } from './problem'
import {
  ADMIN,
  GATED_ADMIN,
  jsonResponse,
  problemResponse,
  renderApp,
  stubFetch,
} from './test-support'

const ME = 'GET /api/v1/auth/me'

describe('safeRedirect', () => {
  it('keeps a same-site path, with its search and hash', () => {
    expect(safeRedirect('/styleguide')).toBe('/styleguide')
    expect(safeRedirect('/boards?filter=mine#top')).toBe('/boards?filter=mine#top')
  })

  it('falls back to the root for an absent or empty target', () => {
    expect(safeRedirect(undefined)).toBe('/')
    expect(safeRedirect('')).toBe('/')
  })

  it('refuses to send the user off-site', () => {
    // `redirect` is attacker-controllable — it is right there in the URL. Without this,
    // Atlas's own login screen becomes a phishing hop: sign in to the real Atlas, get
    // handed to evil.example, having watched the real domain the entire way.
    expect(safeRedirect('https://evil.example')).toBe('/')
    expect(safeRedirect('http://evil.example/x')).toBe('/')
    expect(safeRedirect('javascript:alert(1)')).toBe('/')
  })

  it('refuses a protocol-relative URL, which only looks relative', () => {
    // The one a `startsWith('/')` check waves through. Browsers read it as a full origin.
    expect(safeRedirect('//evil.example')).toBe('/')
    expect(safeRedirect('//evil.example/path')).toBe('/')
  })

  it('refuses a backslash-prefixed URL, which some browsers normalise into one', () => {
    expect(safeRedirect('/\\evil.example')).toBe('/')
  })
})

describe('AuthGate', () => {
  it('shows the login screen to a user with no session', async () => {
    stubFetch({ [ME]: () => problemResponse(PROBLEM_TYPE.unauthorized, 401) })

    renderApp('/')

    expect(await screen.findByRole('heading', { name: 'Log in to Atlas' })).toBeInTheDocument()
    // ...and none of the app leaks out behind it.
    expect(screen.queryByRole('navigation', { name: 'Main' })).not.toBeInTheDocument()
  })

  it('remembers where the user was headed, search and hash included', async () => {
    stubFetch({ [ME]: () => problemResponse(PROBLEM_TYPE.unauthorized, 401) })

    // A deep link into the app: exactly the case where losing the query would land the
    // user on a board with none of the filters they followed the link for.
    const { router } = renderApp('/?filter=mine#top')

    await screen.findByRole('heading', { name: 'Log in to Atlas' })
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/login')
    })
    expect(router.state.location.search).toEqual({ redirect: '/?filter=mine#top' })
  })

  it('renders the app inside its shell for a signed-in user', async () => {
    stubFetch({ [ME]: () => jsonResponse(ADMIN) })

    renderApp('/')

    expect(await screen.findByRole('heading', { name: 'Atlas', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'Main' })).toBeInTheDocument()
  })

  it('does not log the user out when /me fails', async () => {
    // A 500 is not a signed-out user. Redirecting here would sign everybody out of every
    // tab each time the backend restarts, and would do it silently.
    stubFetch({ [ME]: () => new Response('gateway blew up', { status: 502 }) })

    const { router } = renderApp('/')

    expect(await screen.findByRole('heading', { name: 'Atlas is not responding' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Log in to Atlas' })).not.toBeInTheDocument()
    expect(router.state.location.pathname).toBe('/')
  })

  it('recovers when the server comes back', async () => {
    let healthy = false
    stubFetch({
      [ME]: () => (healthy ? jsonResponse(ADMIN) : new Response('down', { status: 502 })),
    })

    renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas is not responding' })

    healthy = true
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }))

    expect(await screen.findByRole('heading', { name: 'Atlas', level: 1 })).toBeInTheDocument()
  })
})

describe('the style guide, the one public route', () => {
  it('renders with no session', async () => {
    stubFetch({ [ME]: () => problemResponse(PROBLEM_TYPE.unauthorized, 401) })

    renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Style guide' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Log in to Atlas' })).not.toBeInTheDocument()
  })

  it('renders even with no backend at all', async () => {
    // It is a static rendering of the design system. e2e/contrast.spec.ts measures computed
    // colour on this page and has never needed a server; that must stay true.
    stubFetch({ [ME]: () => new Response('down', { status: 502 }) })

    renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Style guide' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Atlas is not responding' })).not.toBeInTheDocument()
  })

  it('still gets the shell for a signed-in user', async () => {
    stubFetch({ [ME]: () => jsonResponse(ADMIN) })

    renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Style guide' })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'Main' })).toBeInTheDocument()
  })

  it('is not a way around the forced-reset gate', async () => {
    // The public-route check must run AFTER the gate, never before it.
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    const { router } = renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Choose a password' })).toBeInTheDocument()
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/change-password')
    })
  })
})

describe('the forced-reset gate', () => {
  it('sends a gated user to the change-password screen', async () => {
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    const { router } = renderApp('/')

    expect(await screen.findByRole('heading', { name: 'Choose a password' })).toBeInTheDocument()
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/change-password')
    })
  })

  it('explains WHY, rather than just demanding a password', async () => {
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    renderApp('/')

    expect(
      await screen.findByText('You are signing in with the default credentials'),
    ).toBeInTheDocument()
  })

  it('gives a gated user nothing to click past', async () => {
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    renderApp('/')
    await screen.findByRole('heading', { name: 'Choose a password' })

    // No shell means no nav, no search, no "Create" — there is no escape hatch to find,
    // rather than a set of escape hatches that happen to be disabled.
    expect(screen.queryByRole('navigation', { name: 'Main' })).not.toBeInTheDocument()
    expect(screen.queryByRole('link', { name: /Style guide/ })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Create' })).not.toBeInTheDocument()
  })

  it('bounces a gated user who navigates away by hand', async () => {
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    const { router } = renderApp('/')
    await screen.findByRole('heading', { name: 'Choose a password' })

    // The URL-bar case: no link was clicked, the user just typed a path.
    await router.navigate({ to: '/styleguide' })

    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/change-password')
    })
    expect(screen.getByRole('heading', { name: 'Choose a password' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Style guide' })).not.toBeInTheDocument()
  })

  it('bounces a gated user who deep-links straight into the app', async () => {
    stubFetch({ [ME]: () => jsonResponse(GATED_ADMIN) })

    const { router } = renderApp('/styleguide')

    expect(await screen.findByRole('heading', { name: 'Choose a password' })).toBeInTheDocument()
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/change-password')
    })
  })

  it('lets a gated user reach the login screen only by signing out', async () => {
    // Stateful, because the session really does end: a stub that kept answering `/me` with
    // a signed-in user would have the guard drag the user straight back to the gate, and
    // the test would be asserting against a server that cannot exist.
    let signedIn = true
    stubFetch({
      [ME]: () =>
        signedIn
          ? jsonResponse(GATED_ADMIN)
          : problemResponse(PROBLEM_TYPE.unauthorized, 401),
      'POST /api/v1/auth/logout': () => {
        signedIn = false
        return new Response(null, { status: 204 })
      },
    })

    const { router } = renderApp('/')
    await screen.findByRole('heading', { name: 'Choose a password' })

    await userEvent.click(screen.getByRole('button', { name: 'Sign out instead' }))

    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/login')
    })
  })

  it('reacts to the machine-readable marker, not to the message', async () => {
    // The gate can close under a session that was fine a moment ago. The backend says so
    // with a `type` URN on the 403; this asserts the client acts on that marker.
    let gated = false
    const { calls } = stubFetch({
      [ME]: () => jsonResponse(gated ? GATED_ADMIN : ADMIN),
      'GET /api/v1/auth/sessions': () =>
        problemResponse(PROBLEM_TYPE.passwordChangeRequired, 403),
    })

    const { router, queryClient } = renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    // Something in the app makes a call and is refused by the gate.
    gated = true
    await queryClient.fetchQuery({
      queryKey: ['auth', 'sessions'],
      queryFn: async () => {
        const { api, unwrap } = await import('@/lib/api')
        return unwrap(await api.GET('/api/v1/auth/sessions'))
      },
    }).catch(() => undefined)

    // The marker triggers a /me refetch, which reports the gate, which redirects.
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/change-password')
    })
    expect(calls.filter((call) => call === ME).length).toBeGreaterThan(1)
  })

  it('ignores an ordinary 403, which means something completely different', async () => {
    // Same status, different meaning: "you are not an admin" must NOT send the user to the
    // change-password screen. Only the `type` URN separates the two.
    stubFetch({
      [ME]: () => jsonResponse(ADMIN),
      'GET /api/v1/users': () => problemResponse(PROBLEM_TYPE.forbidden, 403),
    })

    const { router, queryClient } = renderApp('/')
    await screen.findByRole('heading', { name: 'Atlas', level: 1 })

    await queryClient.fetchQuery({
      queryKey: ['users'],
      queryFn: async () => {
        const { api, unwrap } = await import('@/lib/api')
        return unwrap(await api.GET('/api/v1/users'))
      },
    }).catch(() => undefined)

    // Give any errant redirect a chance to happen before declaring it did not.
    await vi.waitFor(() => expect(router.state.location.pathname).toBe('/'))
    expect(screen.getByRole('heading', { name: 'Atlas', level: 1 })).toBeInTheDocument()
  })
})
