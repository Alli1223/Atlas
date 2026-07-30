import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { PillStatus } from './api'
import { STATUS_LABEL } from './status'
import { StatusPill } from './StatusPill'

describe('StatusPill', () => {
  it.each([
    ['valid', 'success'],
    ['expiring', 'moved'],
    ['expired', 'removed'],
    ['invalid', 'removed'],
    ['unchecked', 'default'],
  ] as [PillStatus, string][])(
    'renders %s with the %s (colour) appearance class',
    (status, appearanceClass) => {
      render(<StatusPill status={status} />)
      const pill = screen.getByText(STATUS_LABEL[status])
      // The scoped CSS-module class name contains the appearance token — this is the same
      // assertion the Banner suite uses, and it is what proves the colour actually changed
      // rather than the label alone.
      expect(pill.className).toContain(appearanceClass)
      expect(pill).toHaveAttribute('data-status', status)
    },
  )

  it('gives expired and invalid the same (red) class but valid a different one', () => {
    const { rerender } = render(<StatusPill status="expired" />)
    const expiredClass = screen.getByText(STATUS_LABEL.expired).className

    rerender(<StatusPill status="invalid" />)
    const invalidClass = screen.getByText(STATUS_LABEL.invalid).className

    rerender(<StatusPill status="valid" />)
    const validClass = screen.getByText(STATUS_LABEL.valid).className

    expect(invalidClass).toBe(expiredClass)
    expect(validClass).not.toBe(expiredClass)
  })
})
