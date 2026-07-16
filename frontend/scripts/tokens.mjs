/**
 * Atlas design tokens — the single source of truth for src/styles/tokens.css.
 *
 * Every hex in the RAW PALETTE and SEMANTIC sections is transcribed verbatim from
 * @atlaskit/tokens@15.8.0 (see docs/research/atlassian-design-system.md, which extracted
 * them from the published npm artifacts rather than eyeballing the docs site).
 *
 * Three things that are load-bearing and easy to get wrong:
 *
 *  1. This is the BRAND-REFRESH palette, not the legacy one. Brand blue is Blue700
 *     #1868DB (not the legacy B400 #0052CC) and body text is #292A2E (not navy
 *     #172B4D). Shipping legacy values makes the app look like Jira circa 2022.
 *  2. Semantic `success` resolves to the LIME ramp, not Green. --ds-background-success-bold
 *     is #5B7F24 (Lime700). Reaching for Green on a "Done" lozenge is the single easiest
 *     way to look subtly wrong. Green is only reachable via accent tokens.
 *  3. Dark mode conveys elevation by getting LIGHTER (#18191A sunken -> #1F1F21 base ->
 *     #242528 raised -> #2B2C2F overlay), not by shadow. Porting light's shadow-based
 *     elevation into dark is the classic tell of a hand-rolled dark theme.
 *
 * The light and dark semantic maps are keyed identically and rendered from this one
 * source into all three theme blocks (:root, the prefers-color-scheme media query, and
 * [data-theme="dark"]). tokens.test.ts fails the build if the committed CSS drifts from
 * this file, or if the two maps' key sets diverge.
 *
 * Re-extract with `npm pack @atlaskit/tokens` — do not scrape atlassian.design, it is a
 * JS SPA. Pinned extraction: @atlaskit/tokens@15.8.0.
 */

/** Raw palette. Prefix `--ads-*` marks a raw ramp value; semantics use `--ds-*`. */
export const palette = {
  blue: {
    100: '#E9F2FE', 200: '#CFE1FD', 250: '#ADCBFB', 300: '#8FB8F6',
    400: '#669DF1', 500: '#4688EC', 600: '#357DE8', 700: '#1868DB',
    800: '#1558BC', 850: '#144794', 900: '#123263', 1000: '#1C2B42',
  },
  red: {
    100: '#FFECEB', 200: '#FFD5D2', 250: '#FFB8B2', 300: '#FD9891',
    400: '#F87168', 500: '#F15B50', 600: '#E2483D', 700: '#C9372C',
    800: '#AE2E24', 850: '#872821', 900: '#5D1F1A', 1000: '#42221F',
  },
  green: {
    100: '#DCFFF1', 200: '#BAF3DB', 250: '#97EDC9', 300: '#7EE2B8',
    400: '#4BCE97', 500: '#2ABB7F', 600: '#22A06B', 700: '#1F845A',
    800: '#216E4E', 850: '#19573D', 900: '#164B35', 1000: '#1C3329',
  },
  // `success` semantics resolve to LIME, not green. See header note 2.
  lime: {
    100: '#EFFFD6', 200: '#D3F1A7', 250: '#BDE97C', 300: '#B3DF72',
    400: '#94C748', 500: '#82B536', 600: '#6A9A23', 700: '#5B7F24',
    800: '#4C6B1F', 850: '#3F5224', 900: '#37471F', 1000: '#28311B',
  },
  yellow: {
    100: '#FEF7C8', 200: '#F5E989', 250: '#EFDD4E', 300: '#EED12B',
    400: '#DDB30E', 500: '#CF9F02', 600: '#B38600', 700: '#946F00',
    800: '#7F5F01', 850: '#614A05', 900: '#533F04', 1000: '#332E1B',
  },
  orange: {
    100: '#FFF5DB', 200: '#FCE4A6', 250: '#FBD779', 300: '#FBC828',
    400: '#FCA700', 500: '#F68909', 600: '#E06C00', 700: '#BD5B00',
    800: '#9E4C00', 850: '#7A3B00', 900: '#693200', 1000: '#3A2C1F',
  },
  purple: {
    100: '#F8EEFE', 200: '#EED7FC', 250: '#E3BDFA', 300: '#D8A0F7',
    400: '#C97CF4', 500: '#BF63F3', 600: '#AF59E1', 700: '#964AC0',
    800: '#803FA5', 850: '#673286', 900: '#48245D', 1000: '#35243F',
  },
  teal: {
    100: '#E7F9FF', 200: '#C6EDFB', 250: '#B1E4F7', 300: '#9DD9EE',
    400: '#6CC3E0', 500: '#42B2D7', 600: '#2898BD', 700: '#227D9B',
    800: '#206A83', 850: '#1A5265', 900: '#164555', 1000: '#1E3137',
  },
  magenta: {
    100: '#FFECF8', 200: '#FDD0EC', 250: '#FCB6E1', 300: '#F797D2',
    400: '#E774BB', 500: '#DA62AC', 600: '#CD519D', 700: '#AE4787',
    800: '#943D73', 850: '#77325B', 900: '#50253F', 1000: '#3D2232',
  },
}

/** Light-mode neutrals. The refresh neutrals are warm near-blacks; legacy navy N800 #172B4D is gone. */
export const neutrals = {
  0: '#FFFFFF', 100: '#F8F8F8', 200: '#F0F1F2', 300: '#DDDEE1',
  400: '#B7B9BE', 500: '#8C8F97', 600: '#7D818A', 700: '#6B6E76',
  800: '#505258', 900: '#3B3D42', 1000: '#292A2E', 1100: '#1E1F21',
  1200: '#000000',
}

/** Alpha neutrals — used for borders and subtle fills so they compose over any surface. */
export const neutralAlphas = {
  '100a': '#17171708', '200a': '#0515240F', '300a': '#0B120E24',
  '400a': '#080F214A', '500a': '#050C1F75',
}

/** Dark-mode neutrals. Warm near-black, NOT the old navy #0D1424. */
export const darkNeutrals = {
  '-100': '#111213', 0: '#18191A', 100: '#1F1F21', 200: '#242528',
  250: '#2B2C2F', 300: '#303134', 350: '#3D3F43', 400: '#4B4D51',
  500: '#63666B', 600: '#7E8188', 700: '#96999E', 800: '#A9ABAF',
  900: '#BFC1C4', 1000: '#CECFD2', 1100: '#E2E3E4', 1200: '#FFFFFF',
}

/**
 * Spacing. Note 075 (6px) and 250 (20px) exist — the scale is NOT a pure 4px grid.
 * space.100 (8px) is the workhorse gutter in Jira, not 16px.
 */
export const spacing = {
  0: '0px', '025': '2px', '050': '4px', '075': '6px', 100: '8px',
  150: '12px', 200: '16px', 250: '20px', 300: '24px', 400: '32px',
  500: '40px', 600: '48px', 800: '64px', 1000: '80px',
}

export const negativeSpacing = {
  '025': '-2px', '050': '-4px', '075': '-6px', 100: '-8px',
  150: '-12px', 200: '-16px', 250: '-20px', 300: '-24px', 400: '-32px',
}

export const shape = {
  'radius-xsmall': '2px',
  'radius-small': '4px',
  'radius-medium': '6px',
  'radius-large': '8px',
  'radius-xlarge': '12px',
  'radius-xxlarge': '16px',
  'radius-full': '9999px',
  // Issue-type tiles and square avatars.
  'radius-tile': '25%',
  'border-width': '1px',
  'border-width-selected': '2px',
  'border-width-focused': '2px',
}

/**
 * Typography. Atlassian Sans is Atlassian's derivative of Inter Variable, so Inter is a
 * near-exact free substitute — and font-weight 653 (ADS's bold token) is a real variable
 * axis position Inter honours natively. It silently degrades to 700 on a static font,
 * which visibly coarsens every heading, so the @font-face must declare `font-weight: 100 900`
 * and ship InterVariable.woff2 — not Inter-Bold.woff2.
 *
 * Body is 14px/20px, NOT 16px/24px. This is the density baseline; getting it wrong makes
 * the app read as "not Jira" no matter how correct the colours are.
 */
export const typography = {
  'font-family-body':
    '"Inter","Atlassian Sans",ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",Ubuntu,"Helvetica Neue",sans-serif',
  'font-family-heading': 'var(--ds-font-family-body)',
  'font-family-code':
    '"JetBrains Mono","Atlassian Mono",ui-monospace,Menlo,"Segoe UI Mono","Ubuntu Mono",monospace',

  'font-weight-regular': '400',
  'font-weight-medium': '500',
  'font-weight-semibold': '600',
  'font-weight-bold': '653',

  'font-heading-xxlarge': 'normal 653 32px/36px var(--ds-font-family-heading)',
  'font-heading-xlarge': 'normal 653 28px/32px var(--ds-font-family-heading)',
  'font-heading-large': 'normal 653 24px/28px var(--ds-font-family-heading)',
  'font-heading-medium': 'normal 653 20px/24px var(--ds-font-family-heading)',
  'font-heading-small': 'normal 653 16px/20px var(--ds-font-family-heading)',
  'font-heading-xsmall': 'normal 653 14px/20px var(--ds-font-family-heading)',
  'font-heading-xxsmall': 'normal 653 12px/16px var(--ds-font-family-heading)',
  'font-body-large': 'normal 400 16px/24px var(--ds-font-family-body)',
  'font-body': 'normal 400 14px/20px var(--ds-font-family-body)',
  'font-body-small': 'normal 400 12px/16px var(--ds-font-family-body)',
  'font-code': 'normal 400 0.875em/1 var(--ds-font-family-code)',
}

export const motion = {
  'duration-instant': '0ms',
  'duration-xxshort': '50ms',
  'duration-xshort': '100ms',
  'duration-short': '150ms',
  'duration-medium': '200ms',
  'duration-long': '250ms',
  'duration-xlong': '400ms',
  'duration-xxlong': '600ms',
  'easing-out-practical': 'cubic-bezier(0.4, 1, 0.6, 1)',
  'easing-in-practical': 'cubic-bezier(0.6, 0, 0.8, 0.6)',
  'easing-inout-bold': 'cubic-bezier(0.4, 0, 0, 1)',
  'easing-out-bold': 'cubic-bezier(0, 0.4, 0, 1)',
}

/** Layout constants from @atlaskit/page-layout / @atlaskit/navigation-system. */
export const layout = {
  'topnav-height': '56px',
  'sidenav-width': '240px',
  'sidenav-collapsed-width': '20px',
  'sidenav-min-width': '240px',
  'rightsidebar-width': '280px',
  'panel-width': '368px',
}

/** Light semantic layer. Keys must mirror `dark` exactly — tokens.test.ts enforces it. */
export const light = {
  // ---- elevation surfaces ----
  surface: '#FFFFFF',
  'surface-hovered': '#F0F1F2',
  'surface-pressed': '#DDDEE1',
  'surface-sunken': '#F8F8F8',
  'surface-raised': '#FFFFFF',
  'surface-raised-hovered': '#F0F1F2',
  'surface-raised-pressed': '#DDDEE1',
  'surface-overlay': '#FFFFFF',
  'surface-overlay-hovered': '#F0F1F2',
  'surface-overlay-pressed': '#DDDEE1',

  // ---- elevation shadows ----
  'shadow-raised': '0px 1px 1px #1E1F2140, 0px 0px 1px #1E1F214F',
  'shadow-overlay': '0px 8px 12px #1E1F2126, 0px 0px 1px #1E1F214F',
  'shadow-overflow': '0px 0px 8px #1E1F2129, 0px 0px 1px #1E1F211F',

  // ---- text ----
  text: '#292A2E',
  'text-subtle': '#505258',
  'text-subtlest': '#6B6E76',
  'text-disabled': '#080F214A',
  'text-inverse': '#FFFFFF',
  'text-brand': '#1868DB',
  'text-selected': '#1868DB',
  'text-danger': '#AE2E24',
  'text-warning': '#9E4C00',
  'text-warning-inverse': '#292A2E',
  'text-success': '#4C6B1F',
  'text-discovery': '#803FA5',
  'text-information': '#1558BC',
  link: '#1868DB',
  'link-pressed': '#1558BC',
  'link-visited': '#803FA5',

  // ---- icon ----
  icon: '#292A2E',
  'icon-subtle': '#505258',
  'icon-subtlest': '#6B6E76',
  'icon-disabled': '#080F214A',
  'icon-inverse': '#FFFFFF',
  'icon-brand': '#1868DB',
  'icon-danger': '#C9372C',
  'icon-warning': '#E06C00',
  'icon-success': '#6A9A23',
  'icon-discovery': '#AF59E1',
  'icon-information': '#357DE8',

  // ---- border ----
  border: '#0B120E24',
  'border-bold': '#7D818A',
  'border-input': '#8C8F97',
  'border-disabled': '#0515240F',
  'border-focused': '#4688EC',
  'border-selected': '#1868DB',
  'border-brand': '#1868DB',
  'border-danger': '#E2483D',
  'border-warning': '#E06C00',
  'border-success': '#6A9A23',
  'border-discovery': '#AF59E1',
  'border-information': '#357DE8',
  'border-inverse': '#FFFFFF',

  // ---- background: neutral ----
  'background-neutral': '#0515240F',
  'background-neutral-hovered': '#0B120E24',
  'background-neutral-pressed': '#080F214A',
  'background-neutral-subtle': '#00000000',
  'background-neutral-subtle-hovered': '#0515240F',
  'background-neutral-subtle-pressed': '#0B120E24',
  'background-neutral-bold': '#292A2E',
  'background-neutral-bold-hovered': '#3B3D42',
  'background-neutral-bold-pressed': '#505258',

  // ---- background: selected ----
  'background-selected': '#E9F2FE',
  'background-selected-hovered': '#CFE1FD',
  'background-selected-pressed': '#8FB8F6',
  'background-selected-bold': '#1868DB',

  // ---- background: brand ----
  'background-brand-subtlest': '#E9F2FE',
  'background-brand-subtlest-hovered': '#CFE1FD',
  'background-brand-subtlest-pressed': '#ADCBFB',
  'background-brand-bold': '#1868DB',
  'background-brand-bold-hovered': '#1558BC',
  'background-brand-bold-pressed': '#144794',
  'background-brand-boldest': '#1C2B42',

  // ---- background: danger ----
  'background-danger': '#FFECEB',
  'background-danger-hovered': '#FFD5D2',
  'background-danger-pressed': '#FFB8B2',
  'background-danger-bold': '#C9372C',
  'background-danger-bold-hovered': '#AE2E24',
  'background-danger-bold-pressed': '#872821',

  // ---- background: warning (yellow needs DARK text — see --ds-text-warning-inverse) ----
  'background-warning': '#FFF5DB',
  'background-warning-hovered': '#FCE4A6',
  'background-warning-pressed': '#FBD779',
  'background-warning-bold': '#FBC828',
  'background-warning-bold-hovered': '#FCA700',
  'background-warning-bold-pressed': '#F68909',

  // ---- background: success (LIME ramp) ----
  'background-success': '#EFFFD6',
  'background-success-hovered': '#D3F1A7',
  'background-success-pressed': '#BDE97C',
  'background-success-bold': '#5B7F24',
  'background-success-bold-hovered': '#4C6B1F',
  'background-success-bold-pressed': '#3F5224',

  // ---- background: discovery ----
  'background-discovery': '#F8EEFE',
  'background-discovery-hovered': '#EED7FC',
  'background-discovery-pressed': '#E3BDFA',
  'background-discovery-bold': '#964AC0',
  'background-discovery-bold-hovered': '#803FA5',

  // ---- background: information ----
  'background-information': '#E9F2FE',
  'background-information-hovered': '#CFE1FD',
  'background-information-pressed': '#ADCBFB',
  'background-information-bold': '#357DE8',

  // ---- background: input / disabled ----
  'background-input': '#FFFFFF',
  'background-input-hovered': '#F8F8F8',
  'background-input-pressed': '#FFFFFF',
  'background-disabled': '#0515240F',

  // ---- blanket ----
  blanket: '#050C1F75',
  'blanket-selected': '#388BFF14',
  'blanket-danger': '#EF5C4814',
}

/**
 * Dark semantic layer.
 *
 * The dark shadow values are the one set in this file that ADS does not publish
 * pre-rendered — they are computed from the raw layer objects using ADS's own serializer
 * rule (which discards the base hex's alpha byte and applies the layer opacity), a rule
 * that was validated by byte-matching all three *light* shadows. If dark cards ever look
 * off, check here first.
 */
export const dark = {
  surface: '#1F1F21',
  'surface-hovered': '#242528',
  'surface-pressed': '#2B2C2F',
  'surface-sunken': '#18191A',
  'surface-raised': '#242528',
  'surface-raised-hovered': '#2B2C2F',
  'surface-raised-pressed': '#303134',
  'surface-overlay': '#2B2C2F',
  'surface-overlay-hovered': '#303134',
  'surface-overlay-pressed': '#3D3F43',

  'shadow-raised': '0px 0px 0px 1px #00000000, 0px 1px 1px #01040480, 0px 0px 1px #01040480',
  'shadow-overlay':
    '0px 0px 0px 1px #BDBDBD1F, 0px 8px 12px #0104045C, 0px 0px 1px 1px #01040480',
  'shadow-overflow': '0px 0px 12px #0104048F, 0px 0px 1px #01040480',

  text: '#CECFD2',
  'text-subtle': '#A9ABAF',
  'text-subtlest': '#96999E',
  'text-disabled': '#E5E9F640',
  'text-inverse': '#1F1F21',
  'text-brand': '#669DF1',
  'text-selected': '#669DF1',
  'text-danger': '#FD9891',
  'text-warning': '#FBC828',
  'text-warning-inverse': '#1F1F21',
  'text-success': '#B3DF72',
  'text-discovery': '#D8A0F7',
  'text-information': '#8FB8F6',
  link: '#669DF1',
  'link-pressed': '#8FB8F6',
  'link-visited': '#D8A0F7',

  icon: '#CECFD2',
  'icon-subtle': '#A9ABAF',
  'icon-subtlest': '#96999E',
  'icon-disabled': '#E5E9F640',
  'icon-inverse': '#1F1F21',
  'icon-brand': '#669DF1',
  'icon-danger': '#F15B50',
  'icon-warning': '#FBC828',
  'icon-success': '#82B536',
  'icon-discovery': '#BF63F3',
  'icon-information': '#4688EC',

  border: '#E3E4F21F',
  'border-bold': '#7E8188',
  'border-input': '#7E8188',
  'border-disabled': '#CECED912',
  'border-focused': '#8FB8F6',
  'border-selected': '#669DF1',
  'border-brand': '#669DF1',
  'border-danger': '#F15B50',
  'border-warning': '#F68909',
  'border-success': '#82B536',
  'border-discovery': '#BF63F3',
  'border-information': '#4688EC',
  'border-inverse': '#18191A',

  'background-neutral': '#CECED912',
  'background-neutral-hovered': '#E3E4F21F',
  'background-neutral-pressed': '#E5E9F640',
  'background-neutral-subtle': '#00000000',
  'background-neutral-subtle-hovered': '#CECED912',
  'background-neutral-subtle-pressed': '#E3E4F21F',
  'background-neutral-bold': '#CECFD2',
  'background-neutral-bold-hovered': '#BFC1C4',
  'background-neutral-bold-pressed': '#A9ABAF',

  'background-selected': '#1C2B42',
  'background-selected-hovered': '#123263',
  'background-selected-pressed': '#1558BC',
  'background-selected-bold': '#669DF1',

  'background-brand-subtlest': '#1C2B42',
  'background-brand-subtlest-hovered': '#123263',
  'background-brand-subtlest-pressed': '#144794',
  'background-brand-bold': '#669DF1',
  'background-brand-bold-hovered': '#8FB8F6',
  'background-brand-bold-pressed': '#ADCBFB',
  'background-brand-boldest': '#E9F2FE',

  'background-danger': '#42221F',
  'background-danger-hovered': '#5D1F1A',
  'background-danger-pressed': '#872821',
  'background-danger-bold': '#F87168',
  'background-danger-bold-hovered': '#FD9891',
  'background-danger-bold-pressed': '#FFB8B2',

  'background-warning': '#3A2C1F',
  'background-warning-hovered': '#693200',
  'background-warning-pressed': '#7A3B00',
  'background-warning-bold': '#FBC828',
  'background-warning-bold-hovered': '#FCA700',
  'background-warning-bold-pressed': '#F68909',

  'background-success': '#28311B',
  'background-success-hovered': '#37471F',
  'background-success-pressed': '#3F5224',
  'background-success-bold': '#94C748',
  'background-success-bold-hovered': '#B3DF72',
  'background-success-bold-pressed': '#BDE97C',

  'background-discovery': '#35243F',
  'background-discovery-hovered': '#48245D',
  'background-discovery-pressed': '#673286',
  'background-discovery-bold': '#C97CF4',
  'background-discovery-bold-hovered': '#D8A0F7',

  'background-information': '#1C2B42',
  'background-information-hovered': '#123263',
  'background-information-pressed': '#144794',
  'background-information-bold': '#4688EC',

  'background-input': '#242528',
  'background-input-hovered': '#2B2C2F',
  'background-input-pressed': '#242528',
  'background-disabled': '#E3E4F21F',

  blanket: '#10121499',
  'blanket-selected': '#1D7AFC14',
  'blanket-danger': '#E3493514',
}

/**
 * Accent pairs for tags, labels and avatars — the `--atlas-*` prefix is deliberate:
 * these are DERIVED by Atlas, not extracted from @atlaskit/tokens, and must never be
 * mistaken for real ADS values.
 *
 * The derivation is not invented, though. Every verified semantic pair in ADS follows
 * exactly one rule: light = ramp-100 background + ramp-800 text; dark = ramp-1000
 * background + ramp-300 text. It holds for information (#E9F2FE/#1558BC | #1C2B42/#8FB8F6),
 * danger, success, discovery and warning without exception, so teal/magenta/green/yellow —
 * which have no semantic pair to borrow — are extended by the same rule.
 */
const accentRamps = ['blue', 'teal', 'green', 'lime', 'yellow', 'orange', 'red', 'magenta', 'purple']

function accents(mode) {
  const out = {}
  for (const name of accentRamps) {
    const ramp = palette[name]
    out[`accent-${name}-bg`] = mode === 'light' ? ramp[100] : ramp[1000]
    out[`accent-${name}-text`] = mode === 'light' ? ramp[800] : ramp[300]
  }
  // "standard"/grey defers to the neutral semantics, which already work in both themes.
  out['accent-grey-bg'] = 'var(--ds-background-neutral)'
  out['accent-grey-text'] = 'var(--ds-text)'
  return out
}

export const lightAccents = accents('light')
export const darkAccents = accents('dark')

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

const INDENT = '  '

function block(entries, prefix, indent = INDENT) {
  return Object.entries(entries)
    .map(([k, v]) => `${indent}--${prefix}${k}:${v};`)
    .join('\n')
}

function ramps() {
  const lines = []
  for (const [name, ramp] of Object.entries(palette)) {
    lines.push(
      Object.entries(ramp)
        .map(([stop, hex]) => `${INDENT}--ads-${name}-${stop}:${hex};`)
        .join('\n'),
    )
  }
  lines.push(
    Object.entries(neutrals)
      .map(([stop, hex]) => `${INDENT}--ads-n-${stop}:${hex};`)
      .join('\n'),
  )
  lines.push(
    Object.entries(neutralAlphas)
      .map(([stop, hex]) => `${INDENT}--ads-n-${stop}:${hex};`)
      .join('\n'),
  )
  lines.push(
    Object.entries(darkNeutrals)
      .map(([stop, hex]) => `${INDENT}--ads-dn-${stop}:${hex};`)
      .join('\n'),
  )
  return lines.join('\n\n')
}

/** Renders the complete tokens.css. Pure — tokens.test.ts compares its output to disk. */
export function renderTokensCss() {
  const semanticLight = (indent = INDENT) =>
    [block(light, 'ds-', indent), '', block(lightAccents, 'atlas-', indent)].join('\n')
  const semanticDark = (indent = INDENT) =>
    [block(dark, 'ds-', indent), '', block(darkAccents, 'atlas-', indent)].join('\n')

  return `/* ============================================================
   Atlas design tokens — DO NOT EDIT BY HAND.

   Generated from scripts/tokens.mjs by \`npm run gen:tokens\`.
   src/styles/tokens.test.ts fails if this file drifts from that source.

   Values extracted from @atlaskit/tokens@15.8.0 (brand-refresh palette).
   ============================================================ */

/* Inter Variable (SIL OFL 1.1) is the legitimate free substitute for Atlassian Sans,
   which is Atlassian's own derivative of Inter. The variable range is what makes ADS's
   font-weight: 653 a real axis position rather than a silent round-up to 700. */
@font-face {
  font-family: "Inter";
  font-style: normal;
  font-weight: 100 900;
  font-display: swap;
  src: url("/fonts/InterVariable.woff2") format("woff2");
}

:root {
  color-scheme: light;

  /* ---------- RAW PALETTE (brand refresh) ---------- */
${ramps()}

  /* ---------- SPACING ---------- */
${block(spacing, 'ds-space-')}
${block(negativeSpacing, 'ds-space-negative-')}

  /* ---------- SHAPE ---------- */
${block(shape, 'ds-')}

  /* ---------- TYPOGRAPHY ---------- */
${block(typography, 'ds-')}

  /* ---------- MOTION ---------- */
${block(motion, 'ds-')}

  /* ---------- LAYOUT ---------- */
${block(layout, 'ds-')}

  /* ---------- SEMANTIC: LIGHT (default) ---------- */
${semanticLight()}
}

/* Dark applies when the OS asks for it AND the user has not explicitly chosen light.
   Every dark block below is emitted from one source object, so they cannot drift. */
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;

${semanticDark(INDENT + INDENT)}
  }
}

/* [data-theme] is matched on ANY element, not just :root, so a subtree can be themed
   independently — that is what lets the style guide render both themes side by side.
   An explicit choice must also win in BOTH directions, including light-on-a-dark-OS,
   which is why the light values are repeated here rather than left to :root: an island
   would otherwise inherit dark from the media block above. */
[data-theme="dark"] {
  color-scheme: dark;

${semanticDark()}
}

[data-theme="light"] {
  color-scheme: light;

${semanticLight()}
}
`
}
