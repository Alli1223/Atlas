import '@testing-library/jest-dom/vitest'

import { cleanup } from '@testing-library/react'
import { afterEach, beforeEach, vi } from 'vitest'

/**
 * Node 26 defines its own lazy `localStorage` global that warns and evaluates to
 * `undefined` unless the process was started with --localstorage-file. It shadows the one
 * jsdom would otherwise install on the shared global object, so `localStorage` is
 * undefined inside jsdom tests despite jsdom being loaded correctly.
 *
 * The descriptor is configurable, so we replace it with a real in-memory Storage. Without
 * this, every persistence path silently takes its try/catch fallback and the theme tests
 * would pass for the wrong reason.
 */
class MemoryStorage implements Storage {
  #entries = new Map<string, string>()

  get length(): number {
    return this.#entries.size
  }

  clear(): void {
    this.#entries.clear()
  }

  getItem(key: string): string | null {
    return this.#entries.get(key) ?? null
  }

  key(index: number): string | null {
    return [...this.#entries.keys()][index] ?? null
  }

  removeItem(key: string): void {
    this.#entries.delete(key)
  }

  setItem(key: string, value: string): void {
    this.#entries.set(key, String(value))
  }
}

Object.defineProperty(globalThis, 'localStorage', {
  value: new MemoryStorage(),
  configurable: true,
  writable: true,
})

// jsdom has no layout engine, so scrollTo is unimplemented and the router's scroll
// restoration prints a "Not implemented" error on every navigation. It is noise, not a
// failure — but noise hides real errors, so stub it out.
Object.defineProperty(globalThis, 'scrollTo', {
  value: () => undefined,
  writable: true,
  configurable: true,
})

// jsdom implements no media queries — window.matchMedia is absent entirely. The theme
// layer calls it on every render, so without this stub any test touching the shell throws.
// Tests that care about the system preference override `matches` themselves.
beforeEach(() => {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  )
})

afterEach(() => {
  cleanup()
  localStorage.clear()
  vi.unstubAllGlobals()
  document.documentElement.removeAttribute('data-theme')
})
