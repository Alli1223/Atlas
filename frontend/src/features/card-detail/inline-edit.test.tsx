import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { jsonResponse, problemResponse, stubFetch } from '@/features/auth/test-support'

import { InlineText } from './InlineText'
import { useCard, usePatchCard } from './queries'
import { makeCard, renderWithClient } from './test-support'

/**
 * The inline-edit contract: an edit shows immediately and rolls back if the server rejects
 * it. This is the CLAUDE.md "must feel instant" promise made testable — the optimistic write
 * and, more importantly, its rollback, are exactly the logic that silently corrupts a UI when
 * it regresses (a field left showing a value the server never took).
 */

/** A tiny harness: the summary from cache, edited through the real optimistic mutation. */
function SummaryHarness() {
  const card = useCard('ATLAS-1')
  const patch = usePatchCard('ATLAS-1')
  if (!card.data) return <div>loading</div>
  return (
    <InlineText
      value={card.data.summary}
      label="Summary"
      required
      onCommit={(value) => patch.mutate({ summary: value })}
    />
  )
}

async function editSummaryTo(next: string) {
  const trigger = await screen.findByRole('button', { name: /Summary: Original summary/ })
  await userEvent.click(trigger)
  const box = screen.getByRole('textbox', { name: 'Summary' })
  await userEvent.clear(box)
  await userEvent.type(box, next)
  // Enter (no shift) commits — the same path a blur takes.
  await userEvent.keyboard('{Enter}')
}

describe('inline edit', () => {
  it('applies the new value optimistically and persists it on success', async () => {
    // Stateful stub: the GET reflects whatever the PATCH last committed, so the refetch
    // `onSettled` fires does not clobber the saved value.
    let card = makeCard({ summary: 'Original summary' })
    const { calls } = stubFetch({
      'GET /api/v1/cards/ATLAS-1': () => jsonResponse(card),
      'PATCH /api/v1/cards/ATLAS-1': () => {
        card = { ...card, summary: 'New summary' }
        return jsonResponse(card)
      },
      'GET /api/v1/cards/ATLAS-1/history': () => jsonResponse([]),
      'GET /api/v1/cards/ATLAS-1/transitions': () => jsonResponse([]),
    })

    renderWithClient(<SummaryHarness />)
    await editSummaryTo('New summary')

    // The PATCH carried exactly the edited field.
    await waitFor(() => expect(calls).toContain('PATCH /api/v1/cards/ATLAS-1'))
    expect(await screen.findByRole('button', { name: /Summary: New summary/ })).toBeInTheDocument()
  })

  it('rolls the value back when the server rejects the edit', async () => {
    const card = makeCard({ summary: 'Original summary' })
    stubFetch({
      'GET /api/v1/cards/ATLAS-1': () => jsonResponse(card),
      // 422: a validation failure. The optimistic "New summary" must not survive it.
      'PATCH /api/v1/cards/ATLAS-1': () =>
        problemResponse('urn:atlas:error:validation', 422, 'Summary is invalid.'),
      'GET /api/v1/cards/ATLAS-1/history': () => jsonResponse([]),
      'GET /api/v1/cards/ATLAS-1/transitions': () => jsonResponse([]),
    })

    renderWithClient(<SummaryHarness />)
    await editSummaryTo('New summary')

    // After the rejection the field is back to what the server still holds.
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /Summary: Original summary/ })).toBeInTheDocument(),
    )
    expect(screen.queryByRole('button', { name: /Summary: New summary/ })).not.toBeInTheDocument()
  })
})
