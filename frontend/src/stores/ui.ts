import { create } from 'zustand'

export type ThemePreference = 'light' | 'dark' | 'system'
export type ResolvedTheme = 'light' | 'dark'

/**
 * Shared with the inline no-FOUC script in index.html. Both must agree; if you change
 * this, change index.html — ThemeProvider.test.tsx fails the build if they diverge.
 */
export const THEME_STORAGE_KEY = 'atlas-theme'

export const SIDEBAR_STORAGE_KEY = 'atlas-sidebar-collapsed'

function isThemePreference(value: unknown): value is ThemePreference {
  return value === 'light' || value === 'dark' || value === 'system'
}

/** Reads the persisted preference. Storage can throw (Safari private mode, disabled cookies). */
export function readStoredTheme(): ThemePreference {
  try {
    const stored = localStorage.getItem(THEME_STORAGE_KEY)
    return isThemePreference(stored) ? stored : 'system'
  } catch {
    return 'system'
  }
}

function readStoredSidebar(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_STORAGE_KEY) === 'true'
  } catch {
    return false
  }
}

function persist(key: string, value: string): void {
  try {
    localStorage.setItem(key, value)
  } catch {
    // Non-fatal: the preference simply won't survive a reload.
  }
}

export interface UIState {
  theme: ThemePreference
  isSidebarCollapsed: boolean
  setTheme: (theme: ThemePreference) => void
  toggleSidebar: () => void
  setSidebarCollapsed: (collapsed: boolean) => void
}

/**
 * Client-only UI state. Deliberately small: TanStack Query owns server state and the
 * router owns URL state (board filters, the open card), so what is left is just chrome.
 *
 * Zustand rather than Context because it has selector-based subscriptions — a board with
 * hundreds of nodes must not re-render because the sidebar collapsed.
 */
export const useUI = create<UIState>((set) => ({
  theme: readStoredTheme(),
  isSidebarCollapsed: readStoredSidebar(),
  setTheme: (theme) => {
    persist(THEME_STORAGE_KEY, theme)
    set({ theme })
  },
  toggleSidebar: () =>
    set((state) => {
      const isSidebarCollapsed = !state.isSidebarCollapsed
      persist(SIDEBAR_STORAGE_KEY, String(isSidebarCollapsed))
      return { isSidebarCollapsed }
    }),
  setSidebarCollapsed: (isSidebarCollapsed) => {
    persist(SIDEBAR_STORAGE_KEY, String(isSidebarCollapsed))
    set({ isSidebarCollapsed })
  },
}))
