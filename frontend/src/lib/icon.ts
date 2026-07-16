/**
 * Lucide, calibrated to match ADS.
 *
 * ADS's icons are 16px OUTLINED forms drawn with a 1.5px stroke and SQUARE line caps.
 * (The published glyphs are `fill="currentcolor"` paths, but that is a codegen artifact of
 * flattening strokes — not the design language. ADS and Lucide are the same family, which
 * is exactly why Lucide is the right substitute.)
 *
 * The arithmetic that matters: Lucide draws on a 24 viewBox, so a stroke rendered at
 * `size` scales by size/24. At size 16, Lucide's default strokeWidth of 2 renders at
 * 2 x 16/24 = 1.33px — about 11% thinner than ADS. strokeWidth 2.25 renders at exactly
 * 1.5px. Lucide also defaults to round caps, where ADS uses square.
 *
 * Spread this onto every Lucide icon in UI chrome:  <Bell {...ICON} aria-hidden="true" />
 */
export const ICON = {
  size: 16,
  strokeWidth: 2.25,
  strokeLinecap: 'square',
} as const

/** The 12px variant, for dense contexts like tag chips. Same 1.5px optical stroke. */
export const ICON_SMALL = {
  size: 12,
  strokeWidth: 3,
  strokeLinecap: 'square',
} as const
