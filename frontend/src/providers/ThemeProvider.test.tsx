import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { act, render, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { THEME_STORAGE_KEY, useUI } from '@/stores/ui'

import { resolveTheme, ThemeProvider } from './ThemeProvider'

/** Replaces the setup stub with one reporting a specific system preference. */
function stubMatchMedia(prefersDark: boolean) {
  const listeners = new Set<(e: MediaQueryListEvent) => void>()
  const mql = {
    matches: prefersDark,
    media: '(prefers-color-scheme: dark)',
    onchange: null,
    addEventListener: vi.fn((_: string, cb: (e: MediaQueryListEvent) => void) => {
      listeners.add(cb)
    }),
    removeEventListener: vi.fn((_: string, cb: (e: MediaQueryListEvent) => void) => {
      listeners.delete(cb)
    }),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }
  vi.stubGlobal(
    'matchMedia',
    vi.fn(() => mql),
  )
  return {
    mql,
    /** Simulates the OS flipping theme. */
    emit(nowDark: boolean) {
      mql.matches = nowDark
      for (const cb of listeners) {
        cb({} as MediaQueryListEvent)
      }
    },
  }
}

const theme = () => document.documentElement.dataset.theme

afterEach(() => {
  // The store is module-level state and outlives a single test.
  act(() => {
    useUI.setState({ theme: 'system' })
  })
})

describe('resolveTheme', () => {
  it.each([
    ['light', false, 'light'],
    ['light', true, 'light'],
    ['dark', false, 'dark'],
    ['dark', true, 'dark'],
    ['system', false, 'light'],
    ['system', true, 'dark'],
  ] as const)(
    'resolves %s with systemPrefersDark=%s to %s',
    (preference, prefersDark, expected) => {
      // An explicit choice must beat the OS in BOTH directions — including light-on-dark,
      // which is the case a naive implementation gets wrong.
      expect(resolveTheme(preference, prefersDark)).toBe(expected)
    },
  )
})

describe('ThemeProvider', () => {
  it('writes a resolved theme to <html>, never the literal "system"', () => {
    stubMatchMedia(true)
    act(() => {
      useUI.setState({ theme: 'system' })
    })

    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )

    // The CSS never has to interpret "system" — it only ever sees light or dark.
    expect(theme()).toBe('dark')
  })

  it('follows an explicit light preference on a dark OS', () => {
    stubMatchMedia(true)
    act(() => {
      useUI.setState({ theme: 'light' })
    })

    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )

    expect(theme()).toBe('light')
  })

  it('follows an explicit dark preference on a light OS', () => {
    stubMatchMedia(false)
    act(() => {
      useUI.setState({ theme: 'dark' })
    })

    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )

    expect(theme()).toBe('dark')
  })

  it('reacts to a change of preference', async () => {
    stubMatchMedia(false)
    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )
    expect(theme()).toBe('light')

    act(() => {
      useUI.getState().setTheme('dark')
    })

    await waitFor(() => {
      expect(theme()).toBe('dark')
    })
  })

  it('tracks the OS while the preference is "system"', async () => {
    const media = stubMatchMedia(false)
    act(() => {
      useUI.setState({ theme: 'system' })
    })

    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )
    expect(theme()).toBe('light')

    act(() => {
      media.emit(true)
    })

    await waitFor(() => {
      expect(theme()).toBe('dark')
    })
  })

  it('ignores the OS once the user has chosen explicitly', () => {
    const media = stubMatchMedia(false)
    act(() => {
      useUI.setState({ theme: 'light' })
    })

    render(
      <ThemeProvider>
        <span />
      </ThemeProvider>,
    )

    act(() => {
      media.emit(true)
    })

    expect(theme()).toBe('light')
  })
})

describe('no-FOUC inline script', () => {
  const html = readFileSync(resolve(import.meta.dirname, '../../index.html'), 'utf8')

  it('uses the same storage key as the app', () => {
    // These are two independent implementations of one contract: if the key drifts, the
    // inline script silently stops honouring the saved theme and every reload flashes.
    expect(html).toContain(`localStorage.getItem('${THEME_STORAGE_KEY}')`)
  })

  it('runs before the module bundle, or it cannot prevent the flash', () => {
    expect(html.indexOf('atlas-theme')).toBeLessThan(html.indexOf('src="/src/main.tsx"'))
  })

  it('resolves system preference itself rather than deferring to React', () => {
    expect(html).toContain('prefers-color-scheme: dark')
    expect(html).toContain('documentElement.dataset.theme')
  })

  it('tolerates localStorage throwing', () => {
    // Safari private mode throws on access; an uncaught throw here blanks the whole page
    // because the script runs before anything else.
    expect(html).toMatch(/try\s*\{/)
    expect(html).toMatch(/catch\s*\(/)
  })
})
