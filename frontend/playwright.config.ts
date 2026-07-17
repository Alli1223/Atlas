import { fileURLToPath, URL } from 'node:url'

import { defineConfig, devices } from '@playwright/test'

/**
 * Browser-level tests: the checks jsdom structurally cannot make (computed colour, real
 * layout) and the journeys that are only true end to end (the first-run login).
 *
 * `e2e/` is kept out of the vitest run — vitest owns `src/**` unit tests, playwright owns
 * `e2e/**`. Running a playwright spec under vitest fails confusingly, so the split is by
 * directory rather than by filename.
 *
 * # This suite runs its own stack, on its own ports
 *
 * `auth.spec.ts` is the *first-run* experience, and the seeded `Admin`/`Admin` account
 * exists exactly once per database — the first thing the test does is change that password,
 * so a second run against the same database could not possibly pass. The suite therefore
 * owns a backend with a throwaway database, deleted and re-seeded on every run.
 *
 * That backend cannot be one a developer already has running (its admin password is long
 * since changed), so `reuseExistingServer` is false and the ports are deliberately not the
 * defaults: `npm run test:e2e` is hermetic, and a normal `cargo run` + `npm run dev` on
 * 8080/5173 can stay up beside it.
 */

const API_PORT = 8091
const WEB_PORT = 5174
const WEB_ORIGIN = `http://localhost:${WEB_PORT}`

const REPO_ROOT = fileURLToPath(new URL('..', import.meta.url))
// Under test-results/, which .gitignore already covers.
const DATA_DIR = fileURLToPath(new URL('./test-results/e2e-data', import.meta.url))

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  // Spread rather than `workers: undefined`: exactOptionalPropertyTypes draws a real
  // distinction between an absent key (use the default) and an explicit undefined.
  ...(process.env.CI ? { workers: 1 } : {}),
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : [['list']],
  use: {
    baseURL: WEB_ORIGIN,
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      // `rm -rf` first: a fresh database is the entire point, and `ensure_dirs` recreates
      // the directory on boot. Without this the seeded admin survives from the previous
      // run with a password the test does not know.
      command: `rm -rf '${DATA_DIR}' && cargo run --quiet`,
      cwd: REPO_ROOT,
      env: {
        ATLAS_BIND_ADDR: `127.0.0.1:${API_PORT}`,
        ATLAS_DATA_DIR: DATA_DIR,
        ATLAS_DATABASE_URL: `sqlite://${DATA_DIR}/atlas.db`,
        // The dev proxy sets `changeOrigin`, so the backend sees Host = 127.0.0.1:8091
        // while the browser's Origin is localhost:5174. They can never match, so the
        // browser origin has to be allowlisted explicitly or every POST is a 403 — which
        // presents as "login silently does nothing".
        ATLAS_CORS_ALLOWED_ORIGINS: WEB_ORIGIN,
        // Dev, so the session cookie is not `Secure` — a Secure cookie over plain-HTTP
        // localhost is dropped by the browser without a word, and login would 200 and
        // leave the user signed out.
        ATLAS_ENV: 'dev',
        ATLAS_LOG_LEVEL: 'warn,atlas=info',
      },
      // /healthz pings the database, so this waits for a *migrated* backend rather than
      // just an open port.
      url: `http://127.0.0.1:${API_PORT}/healthz`,
      reuseExistingServer: false,
      // A cold `cargo run` may have to compile.
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
    },
    {
      command: `npm run dev -- --port ${WEB_PORT} --strictPort`,
      env: { ATLAS_API_TARGET: `http://127.0.0.1:${API_PORT}` },
      url: WEB_ORIGIN,
      reuseExistingServer: false,
      timeout: 60_000,
    },
  ],
})
