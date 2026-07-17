import { expect, test } from '@playwright/test'

/**
 * Contrast regression guard.
 *
 * jsdom does not apply stylesheets, so the unit tests cannot see colour at all — a token
 * regression is invisible to them by construction. This suite renders the styleguide in a
 * real browser and asserts measured contrast, which is the only place that check can live.
 *
 * It exists because of a real bug: banner surfaces are bold and theme-independent (warning
 * stays yellow in dark mode) while `--ds-text` flips to near-white, so a default Button in
 * the action slot rendered white-on-yellow at roughly 1.5:1.
 */

/** WCAG 2.1 relative luminance. */
function luminance([r, g, b]: [number, number, number]): number {
  const [rl, gl, bl] = [r, g, b].map((c) => {
    const s = c / 255
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }) as [number, number, number]
  return 0.2126 * rl + 0.7152 * gl + 0.0722 * bl
}

function contrast(fg: [number, number, number], bg: [number, number, number]): number {
  const [a, b] = [luminance(fg), luminance(bg)].sort((x, y) => y - x) as [number, number]
  return (a + 0.05) / (b + 0.05)
}

function parseRgb(css: string): [number, number, number] {
  const m = /rgba?\(([^)]+)\)/.exec(css)
  if (!m?.[1]) throw new Error(`unparseable colour: ${css}`)
  const [r, g, b] = m[1].split(/[,\s/]+/).filter(Boolean).map(Number) as [number, number, number]
  return [r, g, b]
}

test.describe('styleguide contrast', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/styleguide')
    // <html> also carries data-theme, so scope to the styleguide's theme islands via
    // `section` — otherwise the root matches too and each theme silently scans the whole page.
    // Waiting on a real banner also lets React finish: `.count()` does not auto-wait, so
    // asserting on it directly races rendering.
    await expect(page.locator('section[data-theme] [data-appearance]').first()).toBeVisible()
  })

  // The styleguide renders both themes as islands on one page, so one pass covers both.
  for (const theme of ['light', 'dark'] as const) {
    test(`${theme}: banner action buttons stay readable on bold banner surfaces`, async ({ page }) => {
      const island = page.locator(`section[data-theme="${theme}"]`)
      const actions = island.locator('[data-appearance] button')

      const count = await actions.count()
      expect(count, 'styleguide should render banner actions to check').toBeGreaterThan(0)

      for (let i = 0; i < count; i++) {
        const button = actions.nth(i)
        const label = (await button.textContent())?.trim() ?? `#${i}`

        const { fg, bg } = await button.evaluate((el) => {
          const banner = el.closest('[data-appearance]')
          if (!banner) throw new Error('button is not inside a banner')
          return {
            fg: getComputedStyle(el).color,
            // The button's own background is translucent over the banner, so the banner's
            // surface is the real backdrop for the text.
            bg: getComputedStyle(banner).backgroundColor,
          }
        })

        const ratio = contrast(parseRgb(fg), parseRgb(bg))
        expect(
          ratio,
          `${theme} banner action "${label}": ${fg} on ${bg} = ${ratio.toFixed(2)}:1`,
        ).toBeGreaterThanOrEqual(4.5)
      }
    })

    test(`${theme}: banner body text stays readable`, async ({ page }) => {
      const island = page.locator(`section[data-theme="${theme}"]`)
      const banners = island.locator('[data-appearance]')

      const count = await banners.count()
      expect(count).toBeGreaterThan(0)

      for (let i = 0; i < count; i++) {
        const { fg, bg, text } = await banners.nth(i).evaluate((el) => ({
          fg: getComputedStyle(el).color,
          bg: getComputedStyle(el).backgroundColor,
          text: el.textContent?.trim() ?? '',
        }))

        const ratio = contrast(parseRgb(fg), parseRgb(bg))
        expect(ratio, `${theme} banner "${text}": ${fg} on ${bg} = ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5)
      }
    })
  }
})
