import { defineConfig, devices } from '@playwright/test'

/**
 * Browser-level tests. Deliberately narrow for now: it holds the checks that jsdom
 * structurally cannot make (computed colour, real layout). Phase 19 grows this into the
 * full E2E suite.
 *
 * `e2e/` is kept out of the vitest run — vitest owns `src/**` unit tests, playwright owns
 * `e2e/**`. Running a playwright spec under vitest fails confusingly, so the split is by
 * directory rather than by filename.
 */
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
    baseURL: 'http://localhost:5173',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run dev',
    url: 'http://localhost:5173',
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
})
