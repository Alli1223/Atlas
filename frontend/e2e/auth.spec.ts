import { expect, test } from '@playwright/test'

/**
 * The first-run experience, end to end, against the real Axum backend and a real SQLite
 * database seeded exactly as a brand-new instance is.
 *
 * This is the most important test in the phase: it is the literal first thing anyone who
 * installs Atlas does, and every part of it is load-bearing security. The forced-reset gate
 * is the only thing standing between "I started the server" and "anyone on the network is
 * an admin", and it is enforced in two independent places — the backend's `authenticate`
 * layer and the frontend's `AuthGate`. A test that mocked either would be testing neither.
 *
 * # Why this file is serial
 *
 * `Admin`/`Admin` exists once per database, and step one is to destroy it by changing the
 * password. These tests share one backend (see playwright.config.ts), so they must run in
 * order, and only the first may consume the default credentials. `fullyParallel` is on at
 * the project level; this overrides it for this file only.
 */

const DEFAULT_USERNAME = 'Admin'
const DEFAULT_PASSWORD = 'Admin'
const NEW_PASSWORD = 'correct horse battery staple'

test.describe.configure({ mode: 'serial' })

test.describe('first run', () => {
  test('the default admin is forced through a password change before reaching the app', async ({
    page,
  }) => {
    // ---- 1. An unauthenticated visit lands on the login screen, not the app ----
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Log in to Atlas' })).toBeVisible()
    await expect(page).toHaveURL(/\/login/)
    // The shell must not be behind it.
    await expect(page.getByRole('navigation', { name: 'Main' })).toBeHidden()

    // ---- 2. A wrong password is refused, in the backend's own words ----
    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill('definitely not the password')
    await page.getByRole('button', { name: 'Log in' }).click()

    const banner = page.locator('[data-appearance="error"]')
    await expect(banner).toBeVisible()
    // The exact text the backend returns for a bad password, an unknown username and a
    // deactivated account alike — it must not say which.
    await expect(banner).toHaveText(/Invalid username or password/)
    await expect(page).toHaveURL(/\/login/)

    // ---- 3. The seeded credentials get in, and are immediately gated ----
    await page.getByLabel('Password', { exact: true }).fill(DEFAULT_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    await expect(page.getByRole('heading', { name: 'Choose a password' })).toBeVisible()
    await expect(page).toHaveURL(/\/change-password/)

    // ...and it says WHY, which is the difference between a security control and an
    // obstacle.
    await expect(
      page.getByText('You are signing in with the default credentials'),
    ).toBeVisible()

    // ---- 4. There is nothing to click past ----
    await expect(page.getByRole('navigation', { name: 'Main' })).toBeHidden()
    await expect(page.getByRole('button', { name: 'Create' })).toBeHidden()

    // The URL bar is the real escape attempt: no link was clicked, the user just typed a
    // path. This is the assertion the whole gate exists for.
    await page.goto('/')
    await expect(page).toHaveURL(/\/change-password/)
    await expect(page.getByRole('heading', { name: 'Choose a password' })).toBeVisible()

    // Even a route that is public to a signed-out visitor must not be a way around it.
    await page.goto('/styleguide')
    await expect(page).toHaveURL(/\/change-password/)
    await expect(page.getByRole('heading', { name: 'Style guide' })).toBeHidden()

    // ---- 5. The policy is enforced, and the server has the last word ----
    await page.getByLabel('Current password', { exact: true }).fill(DEFAULT_PASSWORD)
    await page.getByLabel('New password', { exact: true }).fill('short')
    await page.getByLabel('Confirm new password', { exact: true }).fill('short')

    const lengthRule = page.locator('[data-rule="length"]')
    await expect(lengthRule).toHaveAttribute('data-satisfied', 'false')
    await expect(page.getByRole('button', { name: 'Set password and continue' })).toBeVisible()

    // Reusing the default is refused by name — the most specific message wins, so an
    // operator who has just been told to change away from "Admin" is told that typing it
    // again is the problem, not that it is too short.
    await page.getByLabel('New password', { exact: true }).fill(DEFAULT_PASSWORD)
    await expect(page.locator('[data-rule="notDefault"]')).toHaveAttribute(
      'data-satisfied',
      'false',
    )

    // ---- 6. A good password: every rule goes green ----
    await page.getByLabel('New password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByLabel('Confirm new password', { exact: true }).fill(NEW_PASSWORD)

    await expect(page.locator('[data-rule="length"]')).toHaveAttribute('data-satisfied', 'true')
    await expect(page.locator('[data-rule="notDefault"]')).toHaveAttribute('data-satisfied', 'true')
    await expect(page.locator('[data-rule="notUsername"]')).toHaveAttribute('data-satisfied', 'true')
    await expect(page.locator('[data-rule="matches"]')).toHaveAttribute('data-satisfied', 'true')

    await page.getByRole('button', { name: 'Set password and continue' }).click()

    // ---- 7. ...and the app opens ----
    await expect(page).toHaveURL(/\/$/)
    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
    // The shell is back: the gate is open.
    await expect(page.getByRole('navigation', { name: 'Main' })).toBeVisible()

    // A reload proves the rotated session cookie is real and was actually stored, rather
    // than the UI simply believing its own optimistic state.
    await page.reload()
    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
    await expect(page).toHaveURL(/\/$/)
  })

  test('the old default password no longer works', async ({ page }) => {
    await page.goto('/login')
    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill(DEFAULT_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    await expect(page.locator('[data-appearance="error"]')).toHaveText(
      /Invalid username or password/,
    )
    await expect(page).toHaveURL(/\/login/)
  })

  test('the new password signs in, and the gate is gone for good', async ({ page }) => {
    await page.goto('/login')
    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
    await expect(page).toHaveURL(/\/$/)
    // must_change_password was cleared server-side, not just in the client's cache.
    await expect(page.getByRole('heading', { name: 'Choose a password' })).toBeHidden()
  })

  test('the username is matched case-insensitively, as the column collation says', async ({
    page,
  }) => {
    await page.goto('/login')
    await page.getByLabel('Username', { exact: true }).fill('admin')
    await page.getByLabel('Password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
  })

  test('a deep link survives the trip through the login screen', async ({ page }) => {
    // Follow a link into the app with no session...
    await page.goto('/?filter=mine')
    await expect(page).toHaveURL(/\/login\?redirect=/)

    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    // ...and end up where you were going, not dumped on the home page.
    await expect(page).toHaveURL(/\/\?filter=mine/)
    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
  })

  test('the login screen will not bounce a user to another site', async ({ page }) => {
    // `redirect` is right there in the URL for an attacker to set. If it were honoured,
    // Atlas's own login page would be a phishing hop: sign in to the real thing, get handed
    // to evil.example, having watched the real domain the whole way.
    await page.goto('/login?redirect=https://evil.example/phish')

    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()

    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()
    await expect(page).toHaveURL(/localhost/)
    await expect(page).not.toHaveURL(/evil\.example/)
  })
})

test.describe('the session cookie', () => {
  test('is HttpOnly, SameSite=Lax, and unreadable from JavaScript', async ({ page, context }) => {
    await page.goto('/login')
    await page.getByLabel('Username', { exact: true }).fill(DEFAULT_USERNAME)
    await page.getByLabel('Password', { exact: true }).fill(NEW_PASSWORD)
    await page.getByRole('button', { name: 'Log in' }).click()
    await expect(page.getByRole('heading', { name: 'Atlas', level: 1, exact: true })).toBeVisible()

    const session = (await context.cookies()).find((cookie) => cookie.name === 'atlas_session')
    expect(session, 'the session cookie should exist after login').toBeDefined()
    // The whole reason Atlas uses a server-side session rather than a localStorage JWT: an
    // XSS cannot read this, so it cannot exfiltrate it.
    expect(session?.httpOnly).toBe(true)
    expect(session?.sameSite).toBe('Lax')

    // Belt and braces: prove it from inside the page, which is where an attacker would be.
    const visible = await page.evaluate(() => document.cookie)
    expect(visible).not.toContain('atlas_session')

    // ...and nothing stashed it somewhere readable instead.
    const stored = await page.evaluate(() => JSON.stringify(window.localStorage))
    expect(stored).not.toContain('atlas_session')
  })
})
