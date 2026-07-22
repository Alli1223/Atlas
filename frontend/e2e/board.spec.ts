import { expect, type Page, test } from '@playwright/test'

/**
 * The board, end to end, against the real Axum backend and a real SQLite database.
 *
 * This is the one place the drag → workflow-transition → optimistic-move path can be
 * exercised for real: PDND builds on native HTML5 drag events, which jsdom does not
 * implement, so the unit suite tests the *reducers* (`applyMove`, `resolveDrop`) and the
 * *mutation lifecycle* (mocked), while the actual pointer-driven drag can only be proven in
 * a browser. Playwright's `dragTo` dispatches the native drag events PDND listens for.
 *
 * # Independence, and why this spec never touches the Admin password
 *
 * The e2e backend seeds only the `Admin` account. `auth.spec.ts` (the first-run test,
 * sharing this database) *owns* that account's password lifecycle: its first test asserts
 * the forced-reset gate and converges the password to a known value. This spec must not
 * race that — if it changed the password itself, auth.spec's gate assertions would fail, and
 * if it used `Admin/Admin` it would hit the gate mid-change.
 *
 * So this spec is a pure *reader* of that settled state: it polls for the post-reset password
 * to start working (auth.spec sets it within seconds of the run starting) and never triggers
 * the gate or writes a password. It then creates its **own** uniquely-keyed project and cards
 * through the browser's authenticated session, so it collides with nothing. This is race-free
 * whenever the full suite runs (which is how `make check` and CI run it — auth.spec is always
 * present to establish the password).
 */

const SETTLED_PASSWORD = 'correct horse battery staple'

/** Attempts one Admin login with the settled password. Returns true once the app opens. */
async function tryLogin(page: Page): Promise<boolean> {
  await page.goto('/login')
  await page.getByLabel('Username', { exact: true }).fill('Admin')
  await page.getByLabel('Password', { exact: true }).fill(SETTLED_PASSWORD)
  await page.getByRole('button', { name: 'Log in' }).click()
  await page
    .waitForURL((url) => !url.pathname.startsWith('/login'), { timeout: 3000 })
    .catch(() => undefined)
  return !page.url().includes('/login')
}

/**
 * Logs in as Admin, waiting for auth.spec to have settled the password. Polls rather than
 * assuming an order, so this spec has no declared dependency on auth.spec yet cannot race it.
 */
async function login(page: Page): Promise<void> {
  const deadline = Date.now() + 45_000
  while (Date.now() < deadline) {
    if (await tryLogin(page)) return
    await page.waitForTimeout(1000)
  }
  throw new Error('Admin login never succeeded — did the first-run auth spec set the password?')
}

interface Setup {
  projectKey: string
  todoCardLabelPrefix: string
}

/**
 * Creates a fresh project and two cards through the API, in the browser's own session, so
 * cookies and Origin are the real ones the CSRF check expects. Returns the project key and
 * the key of a card that starts in To Do.
 */
async function seedProject(page: Page): Promise<Setup> {
  return page.evaluate(async () => {
    const headers = { 'Content-Type': 'application/json' }
    async function post<T>(path: string, body: unknown): Promise<T> {
      const res = await fetch(`/api/v1${path}`, {
        method: 'POST',
        headers,
        credentials: 'include',
        body: JSON.stringify(body),
      })
      if (!res.ok) throw new Error(`POST ${path} → ${res.status}`)
      return (await res.json()) as T
    }
    async function get<T>(path: string): Promise<T> {
      const res = await fetch(`/api/v1${path}`, { credentials: 'include' })
      if (!res.ok) throw new Error(`GET ${path} → ${res.status}`)
      return (await res.json()) as T
    }

    const key = `E2E${Date.now().toString().slice(-6)}`
    await post('/projects', { key, name: `Board E2E ${key}`, template: 'programming' })

    const statuses = await get<{ id: string; name: string }[]>(`/projects/${key}/statuses`)
    const types = await get<{ id: string; isDefault: boolean }[]>(`/projects/${key}/card-types`)
    const todo = statuses.find((s) => s.name === 'To Do')!
    const inProgress = statuses.find((s) => s.name === 'In Progress')!
    const type = (types.find((t) => t.isDefault) ?? types[0])!

    // One card to drag, one card to drop onto (a real drop target in the target column).
    await post(`/projects/${key}/cards`, {
      typeId: type.id,
      summary: 'Drag me across',
      statusId: todo.id,
    })
    await post(`/projects/${key}/cards`, {
      typeId: type.id,
      summary: 'Landing pad',
      statusId: inProgress.id,
    })

    return { projectKey: key, todoCardLabelPrefix: `${key}-1` }
  })
}

/** The header text of the column that currently contains the given card. */
async function columnOf(page: Page, cardKey: string): Promise<string> {
  return page.locator(`[aria-label^="${cardKey}:"]`).first().evaluate((el) => {
    const section = el.closest('section')
    return section?.querySelector('header')?.textContent ?? ''
  })
}

test('a card dragged to another column moves and stays there', async ({ page }) => {
  await login(page)
  const { projectKey } = await seedProject(page)

  await page.goto(`/projects/${projectKey}/board`)

  const dragCard = page.locator(`[aria-label^="${projectKey}-1:"]`).first()
  const landingCard = page.locator(`[aria-label^="${projectKey}-2:"]`).first()
  await expect(dragCard).toBeVisible()

  // The card starts in To Do...
  expect(await columnOf(page, `${projectKey}-1`)).toContain('To Do')

  // ...drag it onto the card sitting in In Progress...
  await dragCard.dragTo(landingCard)

  // ...and it lands — and stays — in In Progress. The optimistic move is confirmed by the
  // server (a permissive workflow move), so a poll here proves it did not snap back.
  await expect
    .poll(() => columnOf(page, `${projectKey}-1`), { timeout: 8000 })
    .toContain('In Progress')

  // A reload proves the move was persisted server-side, not just held in the client cache.
  await page.reload()
  await expect(dragCard).toBeVisible()
  expect(await columnOf(page, `${projectKey}-1`)).toContain('In Progress')
})

test('clicking a card on the board opens its detail modal', async ({ page }) => {
  await login(page)
  const { projectKey } = await seedProject(page)

  await page.goto(`/projects/${projectKey}/board`)

  const card = page.locator(`[aria-label^="${projectKey}-1:"]`).first()
  await expect(card).toBeVisible()

  // No overlay until a card is opened. This is the regression guard: the board sets
  // `?card=KEY` on click, and the route must mount the URL-driven modal in response — a
  // wiring that, once absent, made every board card a dead click.
  await expect(page.getByRole('dialog')).toHaveCount(0)

  await card.click()

  const dialog = page.getByRole('dialog', { name: `Card ${projectKey}-1` })
  await expect(dialog).toBeVisible()
  // The card body really rendered inside the overlay: its summary and a workflow move.
  await expect(dialog.getByText('Drag me across')).toBeVisible()
  await expect(dialog.getByRole('button', { name: /Move to /i }).first()).toBeVisible()

  // The open card lives in the URL, so it survives a reload and is deep-linkable.
  await expect(page).toHaveURL(new RegExp(`card=${projectKey}-1`))
  await page.reload()
  await expect(page.getByRole('dialog', { name: `Card ${projectKey}-1` })).toBeVisible()

  // Escape closes it and clears the param.
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(page).not.toHaveURL(/card=/)
})
