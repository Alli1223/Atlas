import { beforeEach, describe, expect, it, vi } from 'vitest'

import { readStoredTheme, SIDEBAR_STORAGE_KEY, THEME_STORAGE_KEY, useUI } from './ui'

beforeEach(() => {
  localStorage.clear()
  useUI.setState({ theme: 'system', isSidebarCollapsed: false })
})

describe('readStoredTheme', () => {
  it.each(['light', 'dark', 'system'] as const)('reads a stored %s preference', (value) => {
    localStorage.setItem(THEME_STORAGE_KEY, value)
    expect(readStoredTheme()).toBe(value)
  })

  it('defaults to system when nothing is stored', () => {
    expect(readStoredTheme()).toBe('system')
  })

  it('rejects a junk value rather than trusting storage', () => {
    // localStorage is user-writable; a bad value must not reach the DOM as data-theme.
    localStorage.setItem(THEME_STORAGE_KEY, 'chartreuse')
    expect(readStoredTheme()).toBe('system')
  })

  it('survives localStorage throwing', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('SecurityError: storage is disabled')
    })

    expect(readStoredTheme()).toBe('system')

    vi.restoreAllMocks()
  })
})

describe('useUI', () => {
  it('persists the theme', () => {
    useUI.getState().setTheme('dark')

    expect(useUI.getState().theme).toBe('dark')
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark')
  })

  it('toggles the sidebar and persists it', () => {
    expect(useUI.getState().isSidebarCollapsed).toBe(false)

    useUI.getState().toggleSidebar()

    expect(useUI.getState().isSidebarCollapsed).toBe(true)
    expect(localStorage.getItem(SIDEBAR_STORAGE_KEY)).toBe('true')

    useUI.getState().toggleSidebar()

    expect(useUI.getState().isSidebarCollapsed).toBe(false)
    expect(localStorage.getItem(SIDEBAR_STORAGE_KEY)).toBe('false')
  })

  it('sets the sidebar state directly', () => {
    useUI.getState().setSidebarCollapsed(true)

    expect(useUI.getState().isSidebarCollapsed).toBe(true)
    expect(localStorage.getItem(SIDEBAR_STORAGE_KEY)).toBe('true')
  })

  it('still updates state when persistence fails', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('QuotaExceededError')
    })

    useUI.getState().setTheme('dark')

    // The preference just won't survive a reload — it must not break the session.
    expect(useUI.getState().theme).toBe('dark')

    vi.restoreAllMocks()
  })
})
