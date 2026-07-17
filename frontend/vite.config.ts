import { fileURLToPath, URL } from 'node:url'

import babel from '@rolldown/plugin-babel'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import react, { reactCompilerPreset } from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// Vite 8 runs on Rolldown; @vitejs/plugin-react@6 no longer bundles Babel, so the
// React Compiler (1.0.0, stable) is wired through @rolldown/plugin-babel instead of
// the old `react({ babel: { plugins: [...] } })` option, which is a silent no-op here.
//
// `babel()` MUST be listed before `react()`.
// Where the dev proxy sends /api and /ws. Overridable so the Playwright suite can point a
// dev server at its own throwaway backend instead of the one a developer already has on
// 8080 — see playwright.config.ts. NOT VITE_-prefixed: this is read here at config time and
// must never be inlined into the client bundle.
const API_TARGET = process.env.ATLAS_API_TARGET ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [
    tanstackRouter({ target: 'react', autoCodeSplitting: true }),
    babel({ presets: [reactCompilerPreset()] }),
    react(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    port: 5173,
    proxy: {
      // Axum backend. The `/api` prefix is identical on both sides, so no rewrite is
      // needed — that removes a class of "works in dev, 404s in prod" bugs. In prod the
      // Axum binary serves the built assets and this proxy is irrelevant.
      //
      // `changeOrigin: true` rewrites the Host header to the target's, so the backend's
      // origin check can never satisfy itself by comparing Origin to Host in dev — the
      // browser says localhost:5173 and Host says 127.0.0.1:8080. The dev origin must
      // therefore be in ATLAS_CORS_ALLOWED_ORIGINS, which is exactly what its default is.
      '/api': { target: API_TARGET, changeOrigin: true },
      // Live board sync + Claude Code session output.
      // `rewriteWsOrigin` is deliberately NOT set — Vite's docs flag it as a CSRF footgun.
      '/ws': { target: API_TARGET, ws: true },
    },
  },
})
