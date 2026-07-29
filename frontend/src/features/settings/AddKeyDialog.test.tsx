import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { AddKeyDialog } from './AddKeyDialog'
import { credential, jsonResponse, renderWithClient, stubFetch } from './test-support'

/** The exact secret a user types. It must reach the wire once and appear in the DOM never. */
const SECRET = 'ghp_super_secret_token_value_123'

/** Mounts the dialog the way the page does: it unmounts when `onClose` fires. */
function DialogHost() {
  const [open, setOpen] = useState(true)
  if (!open) return <p>closed</p>
  return <AddKeyDialog provider="github" onClose={() => setOpen(false)} />
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('AddKeyDialog', () => {
  it('renders the secret field as a password input, so it is masked even before submit', () => {
    renderWithClient(<AddKeyDialog provider="github" onClose={vi.fn()} />)
    const secretField = screen.getByLabelText(/personal access token/i)
    expect(secretField).toHaveAttribute('type', 'password')
  })

  it('sends the secret exactly once and never echoes it back into the DOM', async () => {
    const user = userEvent.setup()
    const created = credential({ provider: 'github', label: 'work', lastFour: '_123' })
    const { bodies } = stubFetch({
      'POST /api/v1/credentials': () => jsonResponse(created, 201),
    })

    renderWithClient(<DialogHost />)

    await user.type(screen.getByLabelText(/^Label/i), 'work')
    await user.type(screen.getByLabelText(/personal access token/i), SECRET)
    await user.click(screen.getByRole('button', { name: 'Add key' }))

    // The dialog closes on success — the input (and its value) unmounts with it.
    await waitFor(() => expect(screen.getByText('closed')).toBeInTheDocument())

    // The secret went over the wire, once.
    const sentWithSecret = bodies.filter((body) => body.includes(SECRET))
    expect(sentWithSecret).toHaveLength(1)

    // …and it is nowhere in the rendered document. No confirmation, no "you entered", no
    // value left in a detached-but-present node. This is the property the whole vault is
    // built to preserve, at the one screen where the plaintext ever exists client-side.
    expect(document.body.textContent).not.toContain(SECRET)
    expect(document.body.innerHTML).not.toContain(SECRET)
  })

  it('keeps the key out of the DOM even when the server rejects it', async () => {
    const user = userEvent.setup()
    stubFetch({
      'POST /api/v1/credentials': () =>
        jsonResponse(
          { type: 'urn:atlas:error:validation', title: 'Invalid', status: 422, detail: 'nope' },
          422,
        ),
    })

    renderWithClient(<AddKeyDialog provider="github" onClose={vi.fn()} />)

    await user.type(screen.getByLabelText(/^Label/i), 'work')
    await user.type(screen.getByLabelText(/personal access token/i), SECRET)
    await user.click(screen.getByRole('button', { name: 'Add key' }))

    // The error banner shows, but the dialog stays open with the field masked — and the
    // plaintext is never rendered as text anywhere, error path included.
    await waitFor(() => expect(screen.getByText('nope')).toBeInTheDocument())
    expect(document.body.textContent).not.toContain(SECRET)
  })
})
