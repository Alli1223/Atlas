# Atlassian Design System (ADS) — token + component spec for a hand-built Jira-alike in React/TS

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

I skipped the docs site (a JS SPA that WebFetch can't read) and pulled ground truth from the published npm packages: `@atlaskit/tokens@15.8.0` ships raw token JSON for every theme, and the Atlaskit component packages ship `*.compiled.css` with real pixel values. Every hex below is copied from those artifacts, not eyeballed. Two findings dominate the build: (1) **Atlassian Sans is Atlassian's derivative of Inter Variable** — so Inter Variable is a near-exact free substitute, and the odd `font-weight: 653` is a variable-font axis value that works natively in Inter, giving you authentic Jira headings for free. (2) There are **two palettes**, and picking wrong makes everything look dated: the legacy ramp (B400 `#0052CC`, N800 `#172B4D`, G300 `#36B37E`, R400 `#DE350B` — all confirmed) and the current "brand refresh" ramp that Jira actually ships today (brand blue is now Blue700 `#1868DB`, text is `#292A2E` not `#172B4D`). Use the refresh ramp. A trap inside it: **semantic `success` maps to the Lime ramp, not Green** (`--ds-background-success-bold: #5B7F24`), while Green is only reachable via `accent.green`. Density is the other authenticity lever — Jira's base body is 14px/20px with 32px buttons and 11px uppercase lozenges, so a 16px/24px default body will read as "not Jira" no matter how correct the colors are.

## Implementation notes

## Decisions to make before writing code

**1. Use the brand-refresh palette, not legacy.** The prompt asked in legacy names (B400/N800/G300/R400) and I've confirmed all of those, but shipping them will make the app look like Jira circa 2022. Current Jira: brand blue `#1868DB` (not `#0052CC`), body text `#292A2E` (not navy `#172B4D`), dark mode warm-near-black `#1F1F21` (not navy `#0D1424`). The legacy ramp is in `facts` if you want the retro look deliberately.

**2. Font: use Inter Variable.** This is the single highest-leverage finding — Atlassian Sans *is* an Inter derivative, so Inter gets you ~95% there legally and free. Self-host Inter Variable (OFL) and keep ADS's exact `font` shorthands including `font-weight: 653`, which Inter Variable honors as a real axis position. Do **not** substitute Charlie — it's marketing-only and never appears in Jira's UI.

**3. Keep `--ds-*` names verbatim.** They're mechanical (`color.text` → `--ds-text`). Matching them means a later swap to real Atlaskit is a no-op, and you can re-extract updated values any time with `npm pack @atlaskit/tokens`.

**4. Watch the success=Lime trap** — see facts. `--ds-background-success-bold` is `#5B7F24` (lime), not green.

**5. Dark mode elevation is surface-based, not shadow-based.** Light mode lifts cards with shadow; dark mode lifts them by making the surface *lighter* (`#18191A` sunken → `#1F1F21` base → `#242528` raised → `#2B2C2F` overlay). Don't just invert colors and keep the shadows — that's the classic tell of a fake dark theme.

## Density: the authenticity lever

Get this wrong and correct hex codes won't save you. Jira's numbers, all verified:
- **Body is 14px/20px** (not 16px/24px). `body.small` 12px/16px carries most metadata.
- **Buttons 32px** default, 24px compact. **Rows/menu items 28–32px** (min-height 24px + 4px block padding).
- **Lozenges 11px uppercase/16px** — tiny and shouty.
- **Icons 16px** in UI chrome; **avatars 16/24** in dense contexts, 32 elsewhere.
- **8px (`space.100`) is the workhorse gutter**, not 16px.
- Headings are tight: h1 32/36, h2 28/32 — ~1.15 ratio, vs 1.4+ body.

## Draft theme file

Validated with `csstree-validator` (0 errors). Light + dark, `data-theme` overriding `prefers-color-scheme` in both directions.

```css
/* ============================================================
   ADS theme — extracted from @atlaskit/tokens@15.8.0
   Font: Inter Variable (Atlassian Sans is an Inter derivative)
   ============================================================ */

/* Self-host Inter Variable (SIL OFL 1.1) */
@font-face {
  font-family: "Inter";
  font-style: normal;
  font-weight: 100 900;         /* variable range — enables ADS's 653 */
  font-display: swap;
  src: url("/fonts/InterVariable.woff2") format("woff2");
}

:root {
  /* ---------- RAW PALETTE (brand refresh) ---------- */
  --ads-blue-100:#E9F2FE; --ads-blue-200:#CFE1FD; --ads-blue-250:#ADCBFB;
  --ads-blue-300:#8FB8F6; --ads-blue-400:#669DF1; --ads-blue-500:#4688EC;
  --ads-blue-600:#357DE8; --ads-blue-700:#1868DB; --ads-blue-800:#1558BC;
  --ads-blue-850:#144794; --ads-blue-900:#123263; --ads-blue-1000:#1C2B42;

  --ads-red-100:#FFECEB; --ads-red-200:#FFD5D2; --ads-red-250:#FFB8B2;
  --ads-red-300:#FD9891; --ads-red-400:#F87168; --ads-red-500:#F15B50;
  --ads-red-600:#E2483D; --ads-red-700:#C9372C; --ads-red-800:#AE2E24;
  --ads-red-850:#872821; --ads-red-900:#5D1F1A; --ads-red-1000:#42221F;

  --ads-green-100:#DCFFF1; --ads-green-200:#BAF3DB; --ads-green-250:#97EDC9;
  --ads-green-300:#7EE2B8; --ads-green-400:#4BCE97; --ads-green-500:#2ABB7F;
  --ads-green-600:#22A06B; --ads-green-700:#1F845A; --ads-green-800:#216E4E;
  --ads-green-850:#19573D; --ads-green-900:#164B35; --ads-green-1000:#1C3329;

  /* success semantics resolve to LIME, not green */
  --ads-lime-100:#EFFFD6; --ads-lime-200:#D3F1A7; --ads-lime-250:#BDE97C;
  --ads-lime-300:#B3DF72; --ads-lime-400:#94C748; --ads-lime-500:#82B536;
  --ads-lime-600:#6A9A23; --ads-lime-700:#5B7F24; --ads-lime-800:#4C6B1F;
  --ads-lime-850:#3F5224; --ads-lime-900:#37471F; --ads-lime-1000:#28311B;

  --ads-yellow-100:#FEF7C8; --ads-yellow-200:#F5E989; --ads-yellow-250:#EFDD4E;
  --ads-yellow-300:#EED12B; --ads-yellow-400:#DDB30E; --ads-yellow-500:#CF9F02;
  --ads-yellow-600:#B38600; --ads-yellow-700:#946F00; --ads-yellow-800:#7F5F01;
  --ads-yellow-850:#614A05; --ads-yellow-900:#533F04; --ads-yellow-1000:#332E1B;

  --ads-orange-100:#FFF5DB; --ads-orange-200:#FCE4A6; --ads-orange-250:#FBD779;
  --ads-orange-300:#FBC828; --ads-orange-400:#FCA700; --ads-orange-500:#F68909;
  --ads-orange-600:#E06C00; --ads-orange-700:#BD5B00; --ads-orange-800:#9E4C00;
  --ads-orange-850:#7A3B00; --ads-orange-900:#693200; --ads-orange-1000:#3A2C1F;

  --ads-purple-100:#F8EEFE; --ads-purple-200:#EED7FC; --ads-purple-250:#E3BDFA;
  --ads-purple-300:#D8A0F7; --ads-purple-400:#C97CF4; --ads-purple-500:#BF63F3;
  --ads-purple-600:#AF59E1; --ads-purple-700:#964AC0; --ads-purple-800:#803FA5;
  --ads-purple-850:#673286; --ads-purple-900:#48245D; --ads-purple-1000:#35243F;

  --ads-teal-100:#E7F9FF; --ads-teal-200:#C6EDFB; --ads-teal-250:#B1E4F7;
  --ads-teal-300:#9DD9EE; --ads-teal-400:#6CC3E0; --ads-teal-500:#42B2D7;
  --ads-teal-600:#2898BD; --ads-teal-700:#227D9B; --ads-teal-800:#206A83;
  --ads-teal-850:#1A5265; --ads-teal-900:#164555; --ads-teal-1000:#1E3137;

  --ads-magenta-100:#FFECF8; --ads-magenta-200:#FDD0EC; --ads-magenta-250:#FCB6E1;
  --ads-magenta-300:#F797D2; --ads-magenta-400:#E774BB; --ads-magenta-500:#DA62AC;
  --ads-magenta-600:#CD519D; --ads-magenta-700:#AE4787; --ads-magenta-800:#943D73;
  --ads-magenta-850:#77325B; --ads-magenta-900:#50253F; --ads-magenta-1000:#3D2232;

  --ads-n-0:#FFFFFF;    --ads-n-100:#F8F8F8;  --ads-n-200:#F0F1F2;
  --ads-n-300:#DDDEE1;  --ads-n-400:#B7B9BE;  --ads-n-500:#8C8F97;
  --ads-n-600:#7D818A;  --ads-n-700:#6B6E76;  --ads-n-800:#505258;
  --ads-n-900:#3B3D42;  --ads-n-1000:#292A2E; --ads-n-1100:#1E1F21;
  --ads-n-1200:#000000;
  --ads-n-100a:#17171708; --ads-n-200a:#0515240F; --ads-n-300a:#0B120E24;
  --ads-n-400a:#080F214A; --ads-n-500a:#050C1F75;

  --ads-dn--100:#111213; --ads-dn-0:#18191A;   --ads-dn-100:#1F1F21;
  --ads-dn-200:#242528;  --ads-dn-250:#2B2C2F; --ads-dn-300:#303134;
  --ads-dn-350:#3D3F43;  --ads-dn-400:#4B4D51; --ads-dn-500:#63666B;
  --ads-dn-600:#7E8188;  --ads-dn-700:#96999E; --ads-dn-800:#A9ABAF;
  --ads-dn-900:#BFC1C4;  --ads-dn-1000:#CECFD2;--ads-dn-1100:#E2E3E4;
  --ads-dn-1200:#FFFFFF;

  /* ---------- SPACING ---------- */
  --ds-space-0:0px;    --ds-space-025:2px;  --ds-space-050:4px;
  --ds-space-075:6px;  --ds-space-100:8px;  --ds-space-150:12px;
  --ds-space-200:16px; --ds-space-250:20px; --ds-space-300:24px;
  --ds-space-400:32px; --ds-space-500:40px; --ds-space-600:48px;
  --ds-space-800:64px; --ds-space-1000:80px;
  --ds-space-negative-025:-2px;  --ds-space-negative-050:-4px;
  --ds-space-negative-075:-6px;  --ds-space-negative-100:-8px;
  --ds-space-negative-150:-12px; --ds-space-negative-200:-16px;
  --ds-space-negative-250:-20px; --ds-space-negative-300:-24px;
  --ds-space-negative-400:-32px;

  /* ---------- SHAPE ---------- */
  --ds-radius-xsmall:2px; --ds-radius-small:4px;  --ds-radius-medium:6px;
  --ds-radius-large:8px;  --ds-radius-xlarge:12px;--ds-radius-xxlarge:16px;
  --ds-radius-full:9999px;--ds-radius-tile:25%;
  --ds-border-width:1px; --ds-border-width-selected:2px; --ds-border-width-focused:2px;

  /* ---------- TYPOGRAPHY ---------- */
  --ds-font-family-body:"Inter","Atlassian Sans",ui-sans-serif,-apple-system,
    BlinkMacSystemFont,"Segoe UI",Ubuntu,"Helvetica Neue",sans-serif;
  --ds-font-family-heading:var(--ds-font-family-body);
  --ds-font-family-code:"JetBrains Mono","Atlassian Mono",ui-monospace,Menlo,
    "Segoe UI Mono","Ubuntu Mono",monospace;

  --ds-font-weight-regular:400; --ds-font-weight-medium:500;
  --ds-font-weight-semibold:600; --ds-font-weight-bold:653;

  --ds-font-heading-xxlarge: normal 653 32px/36px var(--ds-font-family-heading);
  --ds-font-heading-xlarge:  normal 653 28px/32px var(--ds-font-family-heading);
  --ds-font-heading-large:   normal 653 24px/28px var(--ds-font-family-heading);
  --ds-font-heading-medium:  normal 653 20px/24px var(--ds-font-family-heading);
  --ds-font-heading-small:   normal 653 16px/20px var(--ds-font-family-heading);
  --ds-font-heading-xsmall:  normal 653 14px/20px var(--ds-font-family-heading);
  --ds-font-heading-xxsmall: normal 653 12px/16px var(--ds-font-family-heading);
  --ds-font-body-large:      normal 400 16px/24px var(--ds-font-family-body);
  --ds-font-body:            normal 400 14px/20px var(--ds-font-family-body);
  --ds-font-body-small:      normal 400 12px/16px var(--ds-font-family-body);
  --ds-font-code:            normal 400 0.875em/1 var(--ds-font-family-code);

  /* ---------- MOTION ---------- */
  --ds-duration-instant:0ms;   --ds-duration-xxshort:50ms;
  --ds-duration-xshort:100ms;  --ds-duration-short:150ms;
  --ds-duration-medium:200ms;  --ds-duration-long:250ms;
  --ds-duration-xlong:400ms;   --ds-duration-xxlong:600ms;
  --ds-easing-out-practical:cubic-bezier(0.4, 1, 0.6, 1);
  --ds-easing-in-practical: cubic-bezier(0.6, 0, 0.8, 0.6);
  --ds-easing-inout-bold:   cubic-bezier(0.4, 0, 0, 1);
  --ds-easing-out-bold:     cubic-bezier(0, 0.4, 0, 1);

  /* ---------- LAYOUT (from @atlaskit/page-layout) ---------- */
  --ds-topnav-height:56px;
  --ds-sidenav-width:240px;
  --ds-sidenav-collapsed-width:20px;
  --ds-rightsidebar-width:280px;
  --ds-panel-width:368px;

  /* ============ SEMANTIC — LIGHT ============ */
  --ds-surface:#FFFFFF;
  --ds-surface-hovered:#F0F1F2;
  --ds-surface-pressed:#DDDEE1;
  --ds-surface-sunken:#F8F8F8;
  --ds-surface-raised:#FFFFFF;
  --ds-surface-raised-hovered:#F0F1F2;
  --ds-surface-raised-pressed:#DDDEE1;
  --ds-surface-overlay:#FFFFFF;
  --ds-surface-overlay-hovered:#F0F1F2;
  --ds-surface-overlay-pressed:#DDDEE1;

  --ds-shadow-raised:  0px 1px 1px #1E1F2140, 0px 0px 1px #1E1F214F;
  --ds-shadow-overlay: 0px 8px 12px #1E1F2126, 0px 0px 1px #1E1F214F;
  --ds-shadow-overflow:0px 0px 8px #1E1F2129, 0px 0px 1px #1E1F211F;

  --ds-text:#292A2E;
  --ds-text-subtle:#505258;
  --ds-text-subtlest:#6B6E76;
  --ds-text-disabled:#080F214A;
  --ds-text-inverse:#FFFFFF;
  --ds-text-brand:#1868DB;
  --ds-text-selected:#1868DB;
  --ds-text-danger:#AE2E24;
  --ds-text-warning:#9E4C00;
  --ds-text-warning-inverse:#292A2E;
  --ds-text-success:#4C6B1F;
  --ds-text-discovery:#803FA5;
  --ds-text-information:#1558BC;
  --ds-link:#1868DB;
  --ds-link-pressed:#1558BC;
  --ds-link-visited:#803FA5;

  --ds-icon:#292A2E;
  --ds-icon-subtle:#505258;
  --ds-icon-subtlest:#6B6E76;
  --ds-icon-disabled:#080F214A;
  --ds-icon-inverse:#FFFFFF;
  --ds-icon-brand:#1868DB;
  --ds-icon-danger:#C9372C;
  --ds-icon-warning:#E06C00;
  --ds-icon-success:#6A9A23;
  --ds-icon-discovery:#AF59E1;
  --ds-icon-information:#357DE8;

  --ds-border:#0B120E24;
  --ds-border-bold:#7D818A;
  --ds-border-input:#8C8F97;
  --ds-border-disabled:#0515240F;
  --ds-border-focused:#4688EC;
  --ds-border-selected:#1868DB;
  --ds-border-brand:#1868DB;
  --ds-border-danger:#E2483D;
  --ds-border-warning:#E06C00;
  --ds-border-success:#6A9A23;
  --ds-border-discovery:#AF59E1;
  --ds-border-information:#357DE8;
  --ds-border-inverse:#FFFFFF;

  --ds-background-neutral:#0515240F;
  --ds-background-neutral-hovered:#0B120E24;
  --ds-background-neutral-pressed:#080F214A;
  --ds-background-neutral-subtle:#00000000;
  --ds-background-neutral-subtle-hovered:#0515240F;
  --ds-background-neutral-subtle-pressed:#0B120E24;
  --ds-background-neutral-bold:#292A2E;
  --ds-background-neutral-bold-hovered:#3B3D42;
  --ds-background-neutral-bold-pressed:#505258;

  --ds-background-selected:#E9F2FE;
  --ds-background-selected-hovered:#CFE1FD;
  --ds-background-selected-pressed:#8FB8F6;
  --ds-background-selected-bold:#1868DB;

  --ds-background-brand-subtlest:#E9F2FE;
  --ds-background-brand-subtlest-hovered:#CFE1FD;
  --ds-background-brand-subtlest-pressed:#ADCBFB;
  --ds-background-brand-bold:#1868DB;
  --ds-background-brand-bold-hovered:#1558BC;
  --ds-background-brand-bold-pressed:#144794;
  --ds-background-brand-boldest:#1C2B42;

  --ds-background-danger:#FFECEB;
  --ds-background-danger-hovered:#FFD5D2;
  --ds-background-danger-pressed:#FFB8B2;
  --ds-background-danger-bold:#C9372C;
  --ds-background-danger-bold-hovered:#AE2E24;
  --ds-background-danger-bold-pressed:#872821;

  --ds-background-warning:#FFF5DB;
  --ds-background-warning-hovered:#FCE4A6;
  --ds-background-warning-pressed:#FBD779;
  --ds-background-warning-bold:#FBC828;
  --ds-background-warning-bold-hovered:#FCA700;
  --ds-background-warning-bold-pressed:#F68909;

  --ds-background-success:#EFFFD6;
  --ds-background-success-hovered:#D3F1A7;
  --ds-background-success-pressed:#BDE97C;
  --ds-background-success-bold:#5B7F24;
  --ds-background-success-bold-hovered:#4C6B1F;
  --ds-background-success-bold-pressed:#3F5224;

  --ds-background-discovery:#F8EEFE;
  --ds-background-discovery-hovered:#EED7FC;
  --ds-background-discovery-pressed:#E3BDFA;
  --ds-background-discovery-bold:#964AC0;
  --ds-background-discovery-bold-hovered:#803FA5;

  --ds-background-information:#E9F2FE;
  --ds-background-information-hovered:#CFE1FD;
  --ds-background-information-pressed:#ADCBFB;
  --ds-background-information-bold:#357DE8;

  --ds-background-input:#FFFFFF;
  --ds-background-input-hovered:#F8F8F8;
  --ds-background-input-pressed:#FFFFFF;
  --ds-background-disabled:#0515240F;

  --ds-blanket:#050C1F75;
  --ds-blanket-selected:#388BFF14;
  --ds-blanket-danger:#EF5C4814;
}

/* ============ SEMANTIC — DARK ============ */
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --ds-surface:#1F1F21;
    --ds-surface-hovered:#242528;
    --ds-surface-pressed:#2B2C2F;
    --ds-surface-sunken:#18191A;
    --ds-surface-raised:#242528;
    --ds-surface-raised-hovered:#2B2C2F;
    --ds-surface-raised-pressed:#303134;
    --ds-surface-overlay:#2B2C2F;
    --ds-surface-overlay-hovered:#303134;
    --ds-surface-overlay-pressed:#3D3F43;

    --ds-shadow-raised:  0px 0px 0px 1px #00000000, 0px 1px 1px #01040480, 0px 0px 1px #01040480;
    --ds-shadow-overlay: 0px 0px 0px 1px #BDBDBD1F, 0px 8px 12px #0104045C, 0px 0px 1px 1px #01040480;
    --ds-shadow-overflow:0px 0px 12px #0104048F, 0px 0px 1px #01040480;

    --ds-text:#CECFD2;
    --ds-text-subtle:#A9ABAF;
    --ds-text-subtlest:#96999E;
    --ds-text-disabled:#E5E9F640;
    --ds-text-inverse:#1F1F21;
    --ds-text-brand:#669DF1;
    --ds-text-selected:#669DF1;
    --ds-text-danger:#FD9891;
    --ds-text-warning:#FBC828;
    --ds-text-warning-inverse:#1F1F21;
    --ds-text-success:#B3DF72;
    --ds-text-discovery:#D8A0F7;
    --ds-text-information:#8FB8F6;
    --ds-link:#669DF1;
    --ds-link-pressed:#8FB8F6;
    --ds-link-visited:#D8A0F7;

    --ds-icon:#CECFD2;
    --ds-icon-subtle:#A9ABAF;
    --ds-icon-subtlest:#96999E;
    --ds-icon-disabled:#E5E9F640;
    --ds-icon-inverse:#1F1F21;
    --ds-icon-brand:#669DF1;
    --ds-icon-danger:#F15B50;
    --ds-icon-warning:#FBC828;
    --ds-icon-success:#82B536;
    --ds-icon-discovery:#BF63F3;
    --ds-icon-information:#4688EC;

    --ds-border:#E3E4F21F;
    --ds-border-bold:#7E8188;
    --ds-border-input:#7E8188;
    --ds-border-disabled:#CECED912;
    --ds-border-focused:#8FB8F6;
    --ds-border-selected:#669DF1;
    --ds-border-brand:#669DF1;
    --ds-border-danger:#F15B50;
    --ds-border-warning:#F68909;
    --ds-border-success:#82B536;
    --ds-border-discovery:#BF63F3;
    --ds-border-information:#4688EC;
    --ds-border-inverse:#18191A;

    --ds-background-neutral:#CECED912;
    --ds-background-neutral-hovered:#E3E4F21F;
    --ds-background-neutral-pressed:#E5E9F640;
    --ds-background-neutral-subtle:#00000000;
    --ds-background-neutral-subtle-hovered:#CECED912;
    --ds-background-neutral-subtle-pressed:#E3E4F21F;
    --ds-background-neutral-bold:#CECFD2;
    --ds-background-neutral-bold-hovered:#BFC1C4;
    --ds-background-neutral-bold-pressed:#A9ABAF;

    --ds-background-selected:#1C2B42;
    --ds-background-selected-hovered:#123263;
    --ds-background-selected-pressed:#1558BC;
    --ds-background-selected-bold:#669DF1;

    --ds-background-brand-subtlest:#1C2B42;
    --ds-background-brand-subtlest-hovered:#123263;
    --ds-background-brand-subtlest-pressed:#144794;
    --ds-background-brand-bold:#669DF1;
    --ds-background-brand-bold-hovered:#8FB8F6;
    --ds-background-brand-bold-pressed:#ADCBFB;
    --ds-background-brand-boldest:#E9F2FE;

    --ds-background-danger:#42221F;
    --ds-background-danger-hovered:#5D1F1A;
    --ds-background-danger-pressed:#872821;
    --ds-background-danger-bold:#F87168;
    --ds-background-danger-bold-hovered:#FD9891;
    --ds-background-danger-bold-pressed:#FFB8B2;

    --ds-background-warning:#3A2C1F;
    --ds-background-warning-hovered:#693200;
    --ds-background-warning-pressed:#7A3B00;
    --ds-background-warning-bold:#FBC828;
    --ds-background-warning-bold-hovered:#FCA700;
    --ds-background-warning-bold-pressed:#F68909;

    --ds-background-success:#28311B;
    --ds-background-success-hovered:#37471F;
    --ds-background-success-pressed:#3F5224;
    --ds-background-success-bold:#94C748;
    --ds-background-success-bold-hovered:#B3DF72;
    --ds-background-success-bold-pressed:#BDE97C;

    --ds-background-discovery:#35243F;
    --ds-background-discovery-hovered:#48245D;
    --ds-background-discovery-pressed:#673286;
    --ds-background-discovery-bold:#C97CF4;
    --ds-background-discovery-bold-hovered:#D8A0F7;

    --ds-background-information:#1C2B42;
    --ds-background-information-hovered:#123263;
    --ds-background-information-pressed:#144794;
    --ds-background-information-bold:#4688EC;

    --ds-background-input:#242528;
    --ds-background-input-hovered:#2B2C2F;
    --ds-background-input-pressed:#242528;
    --ds-background-disabled:#E3E4F21F;

    --ds-blanket:#10121499;
    --ds-blanket-selected:#1D7AFC14;
    --ds-blanket-danger:#E3493514;
  }
}

/* Explicit toggle must win in BOTH directions. In real code, generate this
   block from the same source as the @media block — do not hand-maintain twice.
   (Duplicated here only so the snippet is self-contained.) */
:root[data-theme="dark"] {
  --ds-surface:#1F1F21;
  --ds-surface-hovered:#242528;
  --ds-surface-pressed:#2B2C2F;
  --ds-surface-sunken:#18191A;
  --ds-surface-raised:#242528;
  --ds-surface-overlay:#2B2C2F;
  --ds-shadow-raised:  0px 0px 0px 1px #00000000, 0px 1px 1px #01040480, 0px 0px 1px #01040480;
  --ds-shadow-overlay: 0px 0px 0px 1px #BDBDBD1F, 0px 8px 12px #0104045C, 0px 0px 1px 1px #01040480;
  --ds-shadow-overflow:0px 0px 12px #0104048F, 0px 0px 1px #01040480;
  --ds-text:#CECFD2;
  --ds-text-subtle:#A9ABAF;
  --ds-text-subtlest:#96999E;
  --ds-text-inverse:#1F1F21;
  --ds-link:#669DF1;
  --ds-icon:#CECFD2;
  --ds-icon-subtle:#A9ABAF;
  --ds-border:#E3E4F21F;
  --ds-border-focused:#8FB8F6;
  --ds-border-selected:#669DF1;
  --ds-background-neutral:#CECED912;
  --ds-background-neutral-hovered:#E3E4F21F;
  --ds-background-neutral-subtle:#00000000;
  --ds-background-selected:#1C2B42;
  --ds-background-brand-bold:#669DF1;
  --ds-background-danger-bold:#F87168;
  --ds-background-success:#28311B;
  --ds-text-success:#B3DF72;
  --ds-background-information:#1C2B42;
  --ds-text-information:#8FB8F6;
  --ds-blanket:#10121499;
  /* …carry the remaining dark values from the @media block… */
}

/* ============================================================
   COMPONENT RECIPES
   ============================================================ */

body {
  font: var(--ds-font-body);           /* 14px/20px — the density baseline */
  color: var(--ds-text);
  background: var(--ds-surface);
}

h1 { font: var(--ds-font-heading-xxlarge); margin: 0; }
h2 { font: var(--ds-font-heading-xlarge);  margin: 0; }
h3 { font: var(--ds-font-heading-large);   margin: 0; }
h4 { font: var(--ds-font-heading-medium);  margin: 0; }
h5 { font: var(--ds-font-heading-small);   margin: 0; }
h6 { font: var(--ds-font-heading-xsmall);  margin: 0; }

/* focus ring — @atlaskit/focus-ring */
:where(a, button, input, select, textarea, [tabindex]):focus-visible {
  outline: var(--ds-border-width-focused) solid var(--ds-border-focused);
  outline-offset: 2px;
}
.ads-focus-inset:focus-visible { outline-offset: -2px; }

/* Button — 32px default / 24px compact */
.btn {
  display: inline-flex; align-items: center; justify-content: center;
  height: 32px;
  padding-inline: var(--ds-space-150);
  column-gap: var(--ds-space-050);
  border: none;
  border-radius: var(--ds-radius-small);
  font: var(--ds-font-body);
  font-weight: var(--ds-font-weight-medium);
  cursor: pointer;
  transition: background-color var(--ds-duration-short) var(--ds-easing-out-practical);
}
.btn--compact { height: 24px; padding-inline: var(--ds-space-075); }
.btn--icon    { width: 32px; padding-inline: var(--ds-space-075); }

.btn--default  { background: var(--ds-background-neutral); color: var(--ds-text); }
.btn--default:hover  { background: var(--ds-background-neutral-hovered); }
.btn--default:active { background: var(--ds-background-neutral-pressed); }

.btn--primary  { background: var(--ds-background-brand-bold); color: var(--ds-text-inverse); }
.btn--primary:hover  { background: var(--ds-background-brand-bold-hovered); }
.btn--primary:active { background: var(--ds-background-brand-bold-pressed); }

.btn--subtle   { background: var(--ds-background-neutral-subtle); color: var(--ds-text-subtle); }
.btn--subtle:hover  { background: var(--ds-background-neutral-subtle-hovered); }
.btn--subtle:active { background: var(--ds-background-neutral-subtle-pressed); }

.btn--danger   { background: var(--ds-background-danger-bold); color: var(--ds-text-inverse); }
.btn--danger:hover  { background: var(--ds-background-danger-bold-hovered); }
.btn--danger:active { background: var(--ds-background-danger-bold-pressed); }

/* yellow needs DARK text, not white */
.btn--warning  { background: var(--ds-background-warning-bold); color: var(--ds-text-warning-inverse); }
.btn--warning:hover  { background: var(--ds-background-warning-bold-hovered); }

.btn--link {
  background: none; color: var(--ds-link);
  padding-inline: 0; height: auto;
}
.btn--link:hover { text-decoration: underline; }

.btn:disabled {
  background: var(--ds-background-disabled);
  color: var(--ds-text-disabled);
  cursor: not-allowed;
}

/* Lozenge — 11px UPPERCASE, the Jira status pill */
.lozenge {
  display: inline-block; max-width: 200px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  padding-inline: var(--ds-space-050);
  border-radius: var(--ds-radius-small);
  font-family: var(--ds-font-family-body);
  font-size: 11px; line-height: 16px;
  font-weight: var(--ds-font-weight-bold);   /* 653 */
  text-transform: uppercase;
}
/* Jira status categories — mapping confirmed in @atlaskit/lozenge source */
.lozenge--todo        { background: var(--ds-background-neutral);     color: var(--ds-text); }
.lozenge--inprogress  { background: var(--ds-background-information); color: var(--ds-text-information); }
.lozenge--done        { background: var(--ds-background-success);     color: var(--ds-text-success); }
.lozenge--removed     { background: var(--ds-background-danger);      color: var(--ds-text-danger); }
.lozenge--new         { background: var(--ds-background-discovery);   color: var(--ds-text-discovery); }
.lozenge--moved       { background: var(--ds-background-warning);     color: var(--ds-text-warning); }

/* Tag / label chip */
.tag {
  display: inline-flex; align-items: center; gap: var(--ds-space-050);
  padding: var(--ds-space-025) var(--ds-space-050);
  border-radius: var(--ds-radius-small);
  font: var(--ds-font-body-small);
  background: var(--ds-background-neutral);
  color: var(--ds-text);
}
.tag--rounded { border-radius: var(--ds-radius-full); }

/* Avatars */
.avatar { border-radius: var(--ds-radius-full); object-fit: cover; }
.avatar--xsmall { width:16px; height:16px; }
.avatar--small  { width:24px; height:24px; }
.avatar--medium { width:32px; height:32px; }
.avatar--large  { width:40px; height:40px; }
.avatar--stacked { border: 2px solid var(--ds-surface); }

/* Top nav — 56px */
.topnav {
  position: sticky; top: 0; z-index: 10;
  display: flex; align-items: center; gap: var(--ds-space-100);
  height: var(--ds-topnav-height);
  padding-inline: var(--ds-space-150);
  background: var(--ds-surface);
  border-bottom: var(--ds-border-width) solid var(--ds-border);
}

/* Side nav — 240px, collapses to 20px */
.sidenav {
  width: var(--ds-sidenav-width);
  min-width: 240px; max-width: 50vw;         /* ADS resize bounds */
  height: calc(100vh - var(--ds-topnav-height));
  background: var(--ds-surface);
  border-right: var(--ds-border-width) solid var(--ds-border);
  overflow-y: auto;
  transition: width var(--ds-duration-medium) var(--ds-easing-inout-bold);
}
.sidenav[data-collapsed="true"] { width: var(--ds-sidenav-collapsed-width); }

.sidenav__item {                              /* ~32px rows */
  display: flex; align-items: center; gap: var(--ds-space-100);
  min-height: 24px;
  padding: var(--ds-space-075) var(--ds-space-100);
  border-radius: var(--ds-radius-small);
  color: var(--ds-text-subtle);
  font: var(--ds-font-body);
  text-decoration: none;
  transition: background-color var(--ds-duration-xxshort) var(--ds-easing-out-practical);
}
.sidenav__item:hover      { background: var(--ds-background-neutral-subtle-hovered); }
.sidenav__item[aria-current="page"] {
  background: var(--ds-background-selected);
  color: var(--ds-text-selected);
  font-weight: var(--ds-font-weight-medium);
}

/* Board — DERIVED from ADS primitives, not verified against real Jira */
.board {
  display: flex; gap: var(--ds-space-100);
  align-items: flex-start;
  padding: var(--ds-space-100);
  overflow-x: auto;
}
.board__column {
  flex: 0 0 270px; width: 270px;
  background: var(--ds-surface-sunken);
  border-radius: var(--ds-radius-medium);
  padding: var(--ds-space-100);
  max-height: 100%;
}
.board__column-header {
  display: flex; align-items: center; justify-content: space-between;
  padding: var(--ds-space-050) var(--ds-space-050) var(--ds-space-100);
  font: var(--ds-font-body-small);
  font-weight: var(--ds-font-weight-semibold);
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--ds-text-subtlest);
}
.board__cards { display: flex; flex-direction: column; gap: var(--ds-space-100); }

/* Jira card */
.card {
  display: flex; flex-direction: column; gap: var(--ds-space-100);
  padding: var(--ds-space-100) var(--ds-space-150);
  background: var(--ds-surface-raised);
  border-radius: var(--ds-radius-medium);
  box-shadow: var(--ds-shadow-raised);
  cursor: pointer;
  transition: background-color var(--ds-duration-xxshort) var(--ds-easing-out-practical);
}
.card:hover  { background: var(--ds-surface-raised-hovered); }
.card:active { background: var(--ds-surface-raised-pressed); }
.card__title  { font: var(--ds-font-body); color: var(--ds-text); }
.card__footer { display: flex; align-items: center; justify-content: space-between; gap: var(--ds-space-100); }
.card__key    { font: var(--ds-font-body-small); color: var(--ds-text-subtlest); }

/* Issue detail modal — two column */
.modal-blanket {
  position: fixed; inset: 0;
  background: var(--ds-blanket);
  animation: ads-fade-in var(--ds-duration-long) var(--ds-easing-inout-bold);
}
.modal {
  background: var(--ds-surface-overlay);
  border-radius: var(--ds-radius-large);
  box-shadow: var(--ds-shadow-overlay);
  width: min(1024px, calc(100vw - var(--ds-space-800)));
  max-height: calc(100vh - var(--ds-space-800));
  animation: ads-scale-in var(--ds-duration-long) var(--ds-easing-inout-bold);
}
.modal__body {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 280px;   /* main + sidebar */
  gap: var(--ds-space-300);
  padding: var(--ds-space-300);
  overflow-y: auto;
}
@media (max-width: 64rem) { .modal__body { grid-template-columns: 1fr; } }

@keyframes ads-fade-in  { from { opacity: 0; } to { opacity: 1; } }
@keyframes ads-scale-in { from { transform: scale(.95); opacity: 0; } to { transform: none; opacity: 1; } }

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 1ms !important;
    transition-duration: 1ms !important;
  }
}
```

## Icons

ADS's refreshed icons are **16×16 filled paths** with `fill="currentcolor"` (verified from the actual glyph source), which rules out a drop-in match: Lucide is 24×24 stroke-based. Recommendation: **Lucide (ISC licence), rendered at 16px with `stroke-width: 2`** — at 16px a 2px stroke reads at a similar optical density to ADS's filled glyphs. Phosphor's `fill` weight is technically closer in construction but its geometry is rounder and less Jira-like. Set `width/height: 16` and `color: var(--ds-icon-subtle)` and let `currentColor` flow.

**Gaps with no free equivalent** — you must hand-draw these, and they're the ones that actually signal "Jira":
- **Issue-type icons** (Story / Task / Bug / Epic / Subtask). These are the highest-value custom work. Jira draws them as ~16px rounded-square tiles (`--ds-radius-tile: 25%` exists precisely for this) with a white glyph: Task = blue check, Story = green bookmark, Bug = red dot/circle, Epic = purple lightning, Subtask = blue branch arrow. ~30 lines of SVG total.
- **Priority icons** (Highest→Lowest chevron stacks) — trivially drawable with Lucide `chevrons-up`/`chevron-up`/`equal`/`chevron-down`/`chevrons-down`.
- **Product logos / Rovo / Atlassian marks** — these are trademarks. Don't clone them; use your own mark.

## Verification note

Two things I checked rather than assumed, both of which would have shipped wrong values:
1. The dark shadows aren't published as strings anywhere — only as layer objects. I found ADS's serializer (`dist/cjs/utils/color-detection.js`), noticed it **discards the base hex's alpha byte** and uses `opacity` directly, and confirmed the rule by re-rendering all three *light* shadows and byte-matching them against the shipped strings in `token-default-values.js`. A naive alpha-multiply (my first attempt) gave `#01040442` instead of the correct `#0104048F`.
2. The whole CSS block above passes `csstree-validator` with 0 errors.

Extracted artifacts are at `/tmp/claude-1000/-home-alli-Projects-Atlas/a97bfbca-9036-43f7-81ff-59bc6b0b43f3/scratchpad/ads/` if you want to re-derive or diff against a newer version.

## Facts

- **[verified]** Ground truth source: `npm pack @atlaskit/tokens@15.8.0` ships raw token JSON at `dist/cjs/artifacts/tokens-raw/{atlassian-light,atlassian-dark,atlassian-spacing,atlassian-shape,atlassian-typography,atlassian-motion}.js` and palettes at `dist/cjs/artifacts/palettes-raw/{palette,legacy-palette}.js`. Light theme = 447 tokens. This is machine-readable and re-extractable at any time.
  - Evidence: Extracted locally: npm pack @atlaskit/tokens@15.8.0 + node require of the artifacts
- **[verified]** Atlassian Sans is Atlassian's derivative of the Inter Variable typeface. Atlassian's own docs state: "Atlassian Sans is our derivative of the Inter Variable typeface which streamlines the font to optimize for certain type features". Inter Variable is therefore the closest legal free substitute, and `font-weight: 653` (ADS's bold token) is a variable-axis value Inter supports natively.
  - Evidence: https://atlassian.design/foundations/typography/product-typefaces-and-scale
- **[verified]** ADS ships two distinct palettes. LEGACY (confirming the names in the prompt): B400=#0052CC, B300=#0065FF, B500=#0747A6, N800=#172B4D, N900=#091E42, N20=#F4F5F7, G300=#36B37E, G400=#00875A, R400=#DE350B, R300=#FF5630, Y300=#FFAB00, P300=#6554C0, T300=#00B8D9. CURRENT brand-refresh palette uses 100–1000 naming instead and is what Jira ships today.
  - Evidence: dist/cjs/artifacts/palettes-raw/legacy-palette.js and palette.js
- **[verified]** Current (brand-refresh) BLUE ramp: Blue100 #E9F2FE, Blue200 #CFE1FD, Blue250 #ADCBFB, Blue300 #8FB8F6, Blue400 #669DF1, Blue500 #4688EC, Blue600 #357DE8, Blue700 #1868DB, Blue800 #1558BC, Blue850 #144794, Blue900 #123263, Blue1000 #1C2B42. Blue700 #1868DB is the new brand/primary blue (replacing legacy B400 #0052CC).
  - Evidence: palettes-raw/palette.js, category 'blue'
- **[verified]** RED ramp: 100 #FFECEB, 200 #FFD5D2, 250 #FFB8B2, 300 #FD9891, 400 #F87168, 500 #F15B50, 600 #E2483D, 700 #C9372C, 800 #AE2E24, 850 #872821, 900 #5D1F1A, 1000 #42221F.
  - Evidence: palettes-raw/palette.js
- **[verified]** GREEN ramp: 100 #DCFFF1, 200 #BAF3DB, 250 #97EDC9, 300 #7EE2B8, 400 #4BCE97, 500 #2ABB7F, 600 #22A06B, 700 #1F845A, 800 #216E4E, 850 #19573D, 900 #164B35, 1000 #1C3329.
  - Evidence: palettes-raw/palette.js
- **[verified]** LIME ramp: 100 #EFFFD6, 200 #D3F1A7, 250 #BDE97C, 300 #B3DF72, 400 #94C748, 500 #82B536, 600 #6A9A23, 700 #5B7F24, 800 #4C6B1F, 850 #3F5224, 900 #37471F, 1000 #28311B.
  - Evidence: palettes-raw/palette.js
- **[verified]** TRAP: in the brand-refresh theme, semantic `success` resolves to the LIME ramp, not Green. color.background.success.bold=#5B7F24 (Lime700), color.text.success=#4C6B1F (Lime800), color.border.success=#6A9A23 (Lime600), color.icon.success=#6A9A23. The Green ramp is only reachable via the `accent.green.*` tokens. Using Green for success will not match current Jira.
  - Evidence: tokens-raw/atlassian-light.js — success tokens resolve to Lime* originals
- **[verified]** YELLOW ramp: 100 #FEF7C8, 200 #F5E989, 250 #EFDD4E, 300 #EED12B, 400 #DDB30E, 500 #CF9F02, 600 #B38600, 700 #946F00, 800 #7F5F01, 850 #614A05, 900 #533F04, 1000 #332E1B.
  - Evidence: palettes-raw/palette.js
- **[verified]** ORANGE ramp: 100 #FFF5DB, 200 #FCE4A6, 250 #FBD779, 300 #FBC828, 400 #FCA700, 500 #F68909, 600 #E06C00, 700 #BD5B00, 800 #9E4C00, 850 #7A3B00, 900 #693200, 1000 #3A2C1F.
  - Evidence: palettes-raw/palette.js
- **[verified]** PURPLE ramp: 100 #F8EEFE, 200 #EED7FC, 250 #E3BDFA, 300 #D8A0F7, 400 #C97CF4, 500 #BF63F3, 600 #AF59E1, 700 #964AC0, 800 #803FA5, 850 #673286, 900 #48245D, 1000 #35243F.
  - Evidence: palettes-raw/palette.js
- **[verified]** TEAL ramp: 100 #E7F9FF, 200 #C6EDFB, 250 #B1E4F7, 300 #9DD9EE, 400 #6CC3E0, 500 #42B2D7, 600 #2898BD, 700 #227D9B, 800 #206A83, 850 #1A5265, 900 #164555, 1000 #1E3137.
  - Evidence: palettes-raw/palette.js
- **[verified]** MAGENTA ramp: 100 #FFECF8, 200 #FDD0EC, 250 #FCB6E1, 300 #F797D2, 400 #E774BB, 500 #DA62AC, 600 #CD519D, 700 #AE4787, 800 #943D73, 850 #77325B, 900 #50253F, 1000 #3D2232.
  - Evidence: palettes-raw/palette.js
- **[verified]** LIGHT NEUTRAL ramp: N0 #FFFFFF, N100 #F8F8F8, N200 #F0F1F2, N300 #DDDEE1, N400 #B7B9BE, N500 #8C8F97, N600 #7D818A, N700 #6B6E76, N800 #505258, N900 #3B3D42, N1000 #292A2E, N1100 #1E1F21, N1200 #000000. Alpha neutrals: N100A #17171708, N200A #0515240F, N300A #0B120E24, N400A #080F214A, N500A #050C1F75. Note these are the *refresh* neutrals — cool-grey legacy N800 #172B4D is gone; body text is now near-black #292A2E.
  - Evidence: palettes-raw/palette.js, category 'light mode neutral'
- **[verified]** DARK NEUTRAL ramp: DN-100 #111213, DN0 #18191A, DN100 #1F1F21, DN200 #242528, DN250 #2B2C2F, DN300 #303134, DN350 #3D3F43, DN400 #4B4D51, DN500 #63666B, DN600 #7E8188, DN700 #96999E, DN800 #A9ABAF, DN900 #BFC1C4, DN1000 #CECFD2, DN1100 #E2E3E4, DN1200 #FFFFFF. Dark mode is now warm-neutral near-black, NOT the old navy #0D1424.
  - Evidence: palettes-raw/palette.js, category 'dark mode neutral'
- **[verified]** Core semantic tokens (light | dark): color.text #292A2E | #CECFD2; text.subtle #505258 | #A9ABAF; text.subtlest #6B6E76 | #96999E; text.inverse #FFFFFF | #1F1F21; text.brand #1868DB | #669DF1; text.danger #AE2E24 | #FD9891; text.success #4C6B1F | #B3DF72; text.warning #9E4C00 | #FBC828; text.discovery #803FA5 | #D8A0F7; text.information #1558BC | #8FB8F6; link #1868DB | #669DF1; link.visited #803FA5 | #D8A0F7.
  - Evidence: tokens-raw/atlassian-light.js vs atlassian-dark.js
- **[verified]** Border/icon semantics (light | dark): color.border #0B120E24 | #E3E4F21F; border.bold #7D818A | #7E8188; border.focused #4688EC | #8FB8F6; border.input #8C8F97 | #7E8188; border.brand/selected #1868DB | #669DF1; border.danger #E2483D | #F15B50; border.success #6A9A23 | #82B536; border.warning #E06C00 | #F68909; border.discovery #AF59E1 | #BF63F3. icon #292A2E | #CECFD2; icon.subtle #505258 | #A9ABAF.
  - Evidence: tokens-raw/atlassian-light.js vs atlassian-dark.js
- **[verified]** Elevation SURFACES (light | dark): elevation.surface #FFFFFF | #1F1F21; surface.sunken #F8F8F8 | #18191A; surface.raised #FFFFFF | #242528; surface.overlay #FFFFFF | #2B2C2F; surface.hovered #F0F1F2 | #242528; surface.pressed #DDDEE1 | #2B2C2F. Dark mode conveys elevation by getting LIGHTER (#18191A sunken → #1F1F21 base → #242528 raised → #2B2C2F overlay), not by shadow.
  - Evidence: tokens-raw/atlassian-light.js / atlassian-dark.js, group 'paint'
- **[verified]** Elevation SHADOWS, light (these three exactly match the shipped strings in artifacts/token-default-values.js — I verified my renderer against them): shadow.raised = `0px 1px 1px #1E1F2140, 0px 0px 1px #1E1F214F`; shadow.overlay = `0px 8px 12px #1E1F2126, 0px 0px 1px #1E1F214F`; shadow.overflow = `0px 0px 8px #1E1F2129, 0px 0px 1px #1E1F211F`.
  - Evidence: dist/cjs/artifacts/token-default-values.js — exact string match against my computed render
- *[likely]* Elevation SHADOWS, dark (computed from the raw layer objects using ADS's own serializer rule, which I validated reproduces the light values byte-for-byte): shadow.raised = `0px 0px 0px 1px #00000000, 0px 1px 1px #01040480, 0px 0px 1px #01040480`; shadow.overlay = `0px 0px 0px 1px #BDBDBD1F, 0px 8px 12px #0104045C, 0px 0px 1px 1px #01040480`; shadow.overflow = `0px 0px 12px #0104048F, 0px 0px 1px #01040480`.
  - Evidence: Computed from tokens-raw/atlassian-dark.js layer objects; rule taken from dist/cjs/utils/color-detection.js and cross-validated on all 3 light shadows
- **[verified]** ADS's shadow serializer does `hexToRGBAValues(color)` and uses only r/g/b, DISCARDING the base hex's alpha, then applies the layer's `opacity` as the final alpha. This matters for dark, where base colors like #01040475 carry an alpha byte that must be thrown away (a naive multiply gives wrong values).
  - Evidence: dist/cjs/utils/color-detection.js line ~29: `return offset.x+'px '+offset.y+'px '+radius+'px rgba('+r+', '+g+', '+b+', '+opacity+')'`
- **[verified]** SPACING scale (exact): space.0 0px, 025 2px, 050 4px, 075 6px, 100 8px, 150 12px, 200 16px, 250 20px, 300 24px, 400 32px, 500 40px, 600 48px, 800 64px, 1000 80px. Negative variants exist for 025–400. Note 075 (6px) and 250 (20px) exist and are widely used — the scale is not a pure 4px grid.
  - Evidence: tokens-raw/atlassian-spacing.js
- **[verified]** SHAPE scale (exact): radius.xsmall 2px, radius.small 4px, radius.medium 6px, radius.large 8px, radius.xlarge 12px, radius.xxlarge 16px, radius.full 9999px, radius.tile 25%. border.width 1px, border.width.selected 2px, border.width.focused 2px. (Legacy compiled CSS still carries a 3px fallback for radius.small; the token is now 4px.)
  - Evidence: tokens-raw/atlassian-shape.js
- **[verified]** TYPOGRAPHY scale (exact, as shorthand `font` tokens): heading.xxlarge 653 32px/36px; xlarge 653 28px/32px; large 653 24px/28px; medium 653 20px/24px; small 653 16px/20px; xsmall 653 14px/20px; xxsmall 653 12px/16px. body.large 400 16px/24px; body 400 14px/20px; body.small 400 12px/16px. code 400 0.875em/1. Weights: regular 400, medium 500, semibold 600, bold 653.
  - Evidence: tokens-raw/atlassian-typography.js
- **[verified]** Font stacks verbatim: body/heading = `"Atlassian Sans", ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Ubuntu, "Helvetica Neue", sans-serif`; code = `"Atlassian Mono", ui-monospace, Menlo, "Segoe UI Mono", "Ubuntu Mono", monospace`; brand = `"Charlie Display"` / `"Charlie Text"`. Charlie is the MARKETING/brand face — it is NOT used for product UI, so it's irrelevant to a Jira clone.
  - Evidence: tokens-raw/atlassian-typography.js
- **[verified]** MOTION durations: instant 0ms, xxshort 50ms, xshort 100ms, short 150ms, medium 200ms, long 250ms, xlong 400ms, xxlong 600ms. Easings: easing.out.practical cubic-bezier(0.4, 1, 0.6, 1); easing.in.practical cubic-bezier(0.6, 0, 0.8, 0.6); easing.inout.bold cubic-bezier(0.4, 0, 0, 1); easing.out.bold cubic-bezier(0, 0.4, 0, 1).
  - Evidence: tokens-raw/atlassian-motion.js
- **[verified]** Motion recipes: button.hovered/pressed = 150ms cubic-bezier(0.4,1,0.6,1) on background-color. listitem.hovered = 50ms, listitem.pressed/selected = 100ms, both cubic-bezier(0.4,1,0.6,1). modal.enter = 250ms cubic-bezier(0.4,0,0,1) ScaleIn95to100; modal.exit = 200ms cubic-bezier(0.6,0,0.8,0.6). blanket.enter 250ms fade. popup.enter 150ms slide-8px+fade; popup.exit 100ms.
  - Evidence: tokens-raw/atlassian-motion.js
- **[verified]** BUTTON (from @atlaskit/button@24.3.3 compiled CSS): default height 2rem/32px, compact height 1.5rem/24px. padding-inline var(--ds-space-150)=12px; icon-button padding-inline 6px; column-gap 4–6px. font-weight 500 (medium), font body 14px/20px. border-radius radius-small. Icon-only button 24px/32px square.
  - Evidence: btn/package/dist/esm/new-button/variants/shared/button-base.compiled.css — `_4t3iviql{height:2rem}`, `_4t3i1k8s{height:1.5rem}`, `_bozgutpp{padding-inline-start:var(--ds-space-150,9pt)}`
- **[verified]** BUTTON appearance backgrounds (verbatim from compiled CSS): primary = --ds-background-brand-bold #1868DB w/ text-inverse #FFF; danger = --ds-background-danger-bold #C9372C w/ #FFF; warning = --ds-background-warning-bold #FBC828 w/ --ds-text-warning-inverse #292A2E (dark text on yellow); discovery = --ds-background-discovery-bold #964AC0 w/ #FFF; default = --ds-background-neutral #0515240F w/ --ds-text #292A2E; subtle = --ds-background-neutral-subtle #00000000 (transparent) w/ text-subtle #505258; selected = --ds-background-selected #E9F2FE w/ --ds-text-selected #1868DB.
  - Evidence: btn/package/dist/esm/new-button/variants/shared/button-base.compiled.css
- **[verified]** LOZENGE (from @atlaskit/lozenge@14.1.2 legacy compiled CSS — this is the classic Jira status pill): font-size 11px, line-height 16px (`1pc`), text-transform uppercase, font-weight var(--ds-font-weight-bold,653), padding-inline 4px (space-050), border-radius radius-small, default maxWidth 200px with ellipsis.
  - Evidence: lz/package/dist/esm/lozenge.compiled.css — `_1wyb1skh{font-size:11px}`, `_1p1dangw{text-transform:uppercase}`, `_vwz47vkz{line-height:1pc}`, `_k48pwu06{font-weight:var(--ds-font-weight-bold,653)}`
- **[verified]** JIRA STATUS-CATEGORY → lozenge mapping is defined in code: `{default:'neutral', removed:'danger', inprogress:'information', new:'discovery', moved:'warning'}` plus `success`. So: To Do = neutral/grey (bg #0515240F, text #292A2E), In Progress = information/BLUE (bg #E9F2FE, text #1558BC), Done = success/LIME (bg #EFFFD6, text #4C6B1F). This confirms the to-do-grey / in-progress-blue / done-green model, except 'green' is really lime in the refresh.
  - Evidence: lz/package/dist/esm/new/utils.js — legacyAppearanceMap
- **[verified]** AVATAR sizes (exact): xsmall 16, small 24, medium 32, large 40, xlarge 96, xxlarge 128 px. Avatar border width = 2px. Border radius = radius-full (50%) for circle, radius.tile (25%) for square variants.
  - Evidence: av/package/dist/esm/avatar-sizes.js — AVATAR_SIZES object; BORDER_WIDTH = 2
- **[verified]** TAG / label chip (@atlaskit/tag): font body.small 12px/16px, padding-block var(--ds-space-025)=2px, padding-inline var(--ds-space-050)=4px (and a .1875rem/3px variant), border-radius radius-small 4px (rounded variant uses radius-full).
  - Evidence: tg/package/dist/**/*.compiled.css
- **[verified]** MENU / list item density (@atlaskit/menu): min-height 24px, padding-block var(--ds-space-050)=4px or space-100=8px, padding-inline 8/12/16/20px. With 14px/20px body this yields ~28px (compact) to ~36px rows; Jira sidebar nav items sit ~32px.
  - Evidence: mn/package/dist/**/*.compiled.css — `_1tke1tcg{min-height:24px}`
- **[verified]** PAGE LAYOUT constants (@atlaskit/page-layout): DEFAULT_TOP_NAVIGATION_HEIGHT = 56, DEFAULT_LEFT_SIDEBAR_WIDTH = 240, DEFAULT_LEFT_SIDEBAR_FLYOUT_WIDTH = 240, COLLAPSED_LEFT_SIDEBAR_WIDTH = 20 (older: 16), DEFAULT_RIGHT_SIDEBAR_WIDTH = 280, DEFAULT_LEFT/RIGHT_PANEL_WIDTH = 368, DEFAULT_BANNER_HEIGHT = 56, MIN_LEFT_SIDEBAR_DRAG_THRESHOLD = 200. The newer @atlaskit/navigation-system@10.5.5 enforces side-nav resize bounds of min 240px / max 50vw.
  - Evidence: pl/package/dist/esm — constants; nav/package/dist/esm/ui/page-layout/side-nav/side-nav.js `widthResizeBounds = {min:'240px', max:'50vw'}`
- **[verified]** Legacy top nav bar height is 56px, confirmed independently in @atlaskit/atlassian-navigation (`HEIGHT = 56`). In the new navigation-system the top-nav height is injected by the product via CSS var `--n_tNvM` (legacy alias `--topNavigationHeight`), so Jira can vary it; 56px remains the ADS default.
  - Evidence: an/package/dist/esm — `HEIGHT = 56`; nav/package/dist/esm/ui/page-layout/constants.js — topNavMountedVar = '--n_tNvM'
- **[verified]** FOCUS RING (exact, from @atlaskit/focus-ring@4.2.0): `outline: var(--ds-border-width-focused, 2px) solid var(--ds-border-focused, #4688EC); outline-offset: 2px` for outside rings, and `outline-offset: -2px` for inset rings. Dark theme border.focused = #8FB8F6.
  - Evidence: fr/package/dist/esm/focus-ring.js — baseFocusOutsideStyles / baseInsetStyles, BORDER_WIDTH = 2
- **[verified]** Accessibility target: Atlassian specifies WCAG 4.5:1 for regular text and 3:1 for large text and graphics/UI components (WCAG 2.1 AA). ADS additionally ships `atlassian-light-increased-contrast` / `atlassian-dark-increased-contrast` themes, which darken borders/icons (e.g. border.focused #4688EC → #1558BC, border.input #8C8F97 → #505258, border #0B120E24 → #E9F0FB5C).
  - Evidence: https://atlassian.design/foundations/accessibility ; tokens-raw/atlassian-light-increased-contrast.js diffed against atlassian-light.js
- **[verified]** ICONOGRAPHY: @atlaskit/icon@37.0.0 ships 376 core icons. Critically, the 2024-refresh icons are FILLED paths on a 16×16 viewBox using `fill="currentcolor"` — they are NOT stroke-based. Render sizes: small 16px, medium 24px, large 32px, xlarge 48px (compiled CSS confirms 1pc/24px/2pc/3pc = 16/24/32/48).
  - Evidence: ic/package/core/close.js — `<path fill="currentcolor" fill-rule="evenodd" d="m9.06 8 4.97-4.97-1.06-1.06L8 6.94..."/>` on a 16-unit grid; ic/package/dist/esm/constants.js sizes
- *[likely]* Icon substitution reality: Lucide is 24×24 STROKE-based (stroke-width 2, round caps) — a different construction from ADS's 16px filled glyphs. Phosphor ships a `fill` weight and a 256×256 viewBox. Neither reproduces ADS optically; Lucide at 16px with stroke-width 2 is the closest practical match for visual density, and is ISC-licensed (permissive).
  - Evidence: Compared ADS glyph geometry (above) against known Lucide/Phosphor construction
- **[verified]** CSS custom-property naming is `--ds-*`, derived by stripping the leading namespace: color.text → --ds-text; elevation.surface → --ds-surface; color.background.brand.bold → --ds-background-brand-bold; space.100 → --ds-space-100; radius.small → --ds-radius-small; font.body → --ds-font-body; motion.duration.short → --ds-duration-short. 560 names total. Matching these names verbatim keeps a future migration to real Atlaskit trivial.
  - Evidence: dist/cjs/artifacts/token-names.js
- *[uncertain]* Jira board column / card geometry (column ~270–280px wide, 8–12px gap, sunken column background, white raised card, ~8–12px card padding) is PRODUCT-level styling in Jira and is NOT expressed in any ADS package. I could not verify it from source; the CSS below derives these from ADS primitives (surface.sunken for columns, surface.raised + shadow.raised for cards) rather than measuring real Jira.
  - Evidence: Absence: no board/card dimensions in @atlaskit/tokens, page-layout, or navigation-system; not published on atlassian.design

## Risks

- PALETTE CHOICE IS LOAD-BEARING: the prompt's legacy names (B400 #0052CC, N800 #172B4D) belong to the OLD palette. Building with them yields a Jira-circa-2022 look. Current Jira uses brand blue #1868DB and text #292A2E. I defaulted the CSS to the refresh palette — confirm this matches the Jira you're targeting before implementing, because it's expensive to swap later.
- SUCCESS = LIME, NOT GREEN. --ds-background-success-bold is #5B7F24 (Lime700) and --ds-text-success is #4C6B1F (Lime800). Reaching for the Green ramp for 'Done' lozenges is the single easiest way to look subtly wrong. Green is only used via accent.green.*.
- Board column geometry (270px width, 8px gap, sunken bg) and card padding/radius are DERIVED from ADS primitives, not verified — Jira's board is product code that ships in no public package and isn't documented on atlassian.design. Everything else in this report is extracted from source. If board fidelity matters, measure a real Jira instance in DevTools; treat my numbers as a starting point.
- Atlassian Sans, Atlassian Mono, and Charlie are proprietary and gated behind authenticated download. Do not fetch them from font-piracy mirrors like onlinewebfonts/freeforfonts that surfaced in search — that's a licensing problem, not a technical one. Inter Variable (OFL) is the legitimate path and is genuinely near-identical since Atlassian Sans is derived from it. Keep "Atlassian Sans" first in the stack only if you expect Atlassian employees to view it; otherwise drop it.
- font-weight: 653 silently degrades to 700 on any non-variable font. If Inter Variable fails to load or you fall back to a static Inter, every heading gets heavier and the Jira feel drops. Verify the @font-face declares `font-weight: 100 900` and that you're shipping InterVariable.woff2, not Inter-Bold.woff2.
- Jira issue-type icons, priority icons, and status glyphs have no free equivalent and are the strongest visual signal of authenticity. Budget hand-drawn SVG time for these — Lucide alone will not get you there. Avoid cloning Atlassian's actual product logos/marks (trademark).
- The dark-theme shadow values are computed, not copied from a shipped string (ADS publishes only light shadows pre-rendered). I validated the derivation rule against all three light shadows, so confidence is high, but if dark cards look off, this is the first place to check.
- Dark mode signals elevation via lighter surfaces (#18191A→#1F1F21→#242528→#2B2C2F), not shadows. Porting the light theme's shadow-based elevation into dark is the classic tell of a hand-rolled dark mode.
- The @media (prefers-color-scheme: dark) block and the [data-theme="dark"] block duplicate values in my snippet so it stands alone. In real code, generate both from one source (a TS token object or a Sass mixin) — hand-maintaining two copies guarantees drift.
- @atlaskit/tokens is versioned and Atlassian revises the palette (the legacy→refresh change proves it). Pin your extraction to 15.8.0 and record it, or you'll silently diverge. Re-extract with `npm pack @atlaskit/tokens` rather than re-scraping the docs site, which is a JS SPA that WebFetch cannot read.
