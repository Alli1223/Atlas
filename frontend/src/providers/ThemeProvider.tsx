import { type ReactNode, useEffect } from 'react'

import { type ResolvedTheme, type ThemePreference, useUI } from '@/stores/ui'

const DARK_QUERY = '(prefers-color-scheme: dark)'

export function resolveTheme(preference: ThemePreference, systemPrefersDark: boolean): ResolvedTheme {
  if (preference === 'system') {
    return systemPrefersDark ? 'dark' : 'light'
  }
  return preference
}

function systemPrefersDark(): boolean {
  return typeof window !== 'undefined' && window.matchMedia(DARK_QUERY).matches
}

export interface ThemeProviderProps {
  children: ReactNode
}

/**
 * Applies the theme to <html data-theme>.
 *
 * The attribute is always set to a *resolved* value ('light' | 'dark') while the stored
 * preference may be 'system' — so the CSS never has to guess, and an explicit choice wins
 * over the OS in both directions. The prefers-color-scheme block in tokens.css is then
 * only a backstop for the no-JS case.
 *
 * First paint is handled by the inline script in index.html, not here: a useEffect runs
 * after paint, which is exactly one frame of white flash on a dark-themed load.
 */
export function ThemeProvider({ children }: ThemeProviderProps) {
  const theme = useUI((state) => state.theme)

  useEffect(() => {
    const media = window.matchMedia(DARK_QUERY)

    const apply = () => {
      document.documentElement.dataset.theme = resolveTheme(theme, media.matches)
    }

    apply()

    // Only 'system' needs to track the OS; an explicit choice ignores it.
    if (theme !== 'system') {
      return undefined
    }

    media.addEventListener('change', apply)
    return () => {
      media.removeEventListener('change', apply)
    }
  }, [theme])

  return children
}

/** Convenience hook: the preference, the resolved value, and a setter. */
export function useTheme(): {
  theme: ThemePreference
  resolvedTheme: ResolvedTheme
  setTheme: (theme: ThemePreference) => void
} {
  const theme = useUI((state) => state.theme)
  const setTheme = useUI((state) => state.setTheme)
  return { theme, resolvedTheme: resolveTheme(theme, systemPrefersDark()), setTheme }
}
