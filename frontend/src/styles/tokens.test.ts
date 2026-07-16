import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { describe, expect, it } from 'vitest'

import { dark, light, renderTokensCss } from '../../scripts/tokens.mjs'

const TOKENS_CSS = resolve(import.meta.dirname, 'tokens.css')

const css = (): string => readFileSync(TOKENS_CSS, 'utf8')

describe('tokens.css', () => {
  it('is in sync with scripts/tokens.mjs', () => {
    // If this fails, run `npm run gen:tokens`. Someone hand-edited the generated file,
    // which is how the light and dark themes start drifting apart.
    expect(css()).toBe(renderTokensCss())
  })

  it('defines every semantic token in both themes', () => {
    // The real failure mode this catches: adding a token to light and forgetting dark,
    // which shows up as an invisible element on a dark background months later.
    expect(Object.keys(dark).sort()).toEqual(Object.keys(light).sort())
  })

  it('emits dark values for prefers-color-scheme AND an explicit data-theme', () => {
    const text = css()
    expect(text).toContain('@media (prefers-color-scheme: dark)')
    // Dark applies unless the user explicitly asked for light...
    expect(text).toContain(':root:not([data-theme="light"])')
    // ...and an explicit choice wins in both directions.
    expect(text).toContain('[data-theme="dark"]')
    expect(text).toContain('[data-theme="light"]')
  })

  it('renders identical dark values in the media query and the data-theme block', () => {
    const text = css()
    const mediaBlock = text.slice(
      text.indexOf(':root:not([data-theme="light"])'),
      text.indexOf('[data-theme="dark"]'),
    )
    const attrBlock = text.slice(text.indexOf('[data-theme="dark"]'), text.indexOf('[data-theme="light"] {'))

    const surfaceIn = (block: string) => /--ds-surface:(#[0-9A-F]{6})/.exec(block)?.[1]
    expect(surfaceIn(mediaBlock)).toBe('#1F1F21')
    expect(surfaceIn(attrBlock)).toBe('#1F1F21')
  })
})

describe('ADS fidelity', () => {
  it('uses the brand-refresh palette, not the legacy one', () => {
    const text = css()
    // Brand blue is Blue700, and body text is a warm near-black.
    expect(light['background-brand-bold']).toBe('#1868DB')
    expect(light.text).toBe('#292A2E')
    // Legacy values would make this look like Jira circa 2022.
    expect(text).not.toContain('#0052CC') // legacy B400
    expect(text).not.toContain('#172B4D') // legacy N800
  })

  it('maps success to the LIME ramp, not green', () => {
    // The single easiest way to look subtly wrong is a green "Done" lozenge.
    expect(light['background-success-bold']).toBe('#5B7F24') // Lime700
    expect(light['text-success']).toBe('#4C6B1F') // Lime800
    expect(light['background-success']).toBe('#EFFFD6') // Lime100
    // Green ramp values must not leak into success semantics.
    expect(light['background-success-bold']).not.toBe('#1F845A') // Green700
  })

  it('conveys dark elevation with lighter surfaces, not shadows', () => {
    expect(dark['surface-sunken']).toBe('#18191A')
    expect(dark.surface).toBe('#1F1F21')
    expect(dark['surface-raised']).toBe('#242528')
    expect(dark['surface-overlay']).toBe('#2B2C2F')
  })

  it('keeps dark text on the yellow warning background', () => {
    // White on #FBC828 fails contrast; ADS ships a dedicated inverse token for this.
    expect(light['background-warning-bold']).toBe('#FBC828')
    expect(light['text-warning-inverse']).toBe('#292A2E')
  })

  it('holds the Jira density baseline', () => {
    const text = css()
    expect(text).toContain('--ds-font-body:normal 400 14px/20px') // NOT 16px/24px
    expect(text).toContain('--ds-space-100:8px') // the workhorse gutter
    expect(text).toContain('--ds-font-weight-bold:653') // a real Inter variable axis
    expect(text).toContain('--ds-topnav-height:56px')
    expect(text).toContain('--ds-sidenav-width:240px')
  })

  it('declares Inter Variable across the full weight range', () => {
    // font-weight: 653 silently degrades to 700 on a static font, coarsening every heading.
    const text = css()
    expect(text).toContain('font-weight: 100 900')
    expect(text).toContain('InterVariable.woff2')
  })
})
