import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createRef } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { Checkbox } from './Checkbox'

describe('Checkbox', () => {
  it('associates the label with the control', async () => {
    render(<Checkbox label="Notify me" />)

    // Clicking the label text must toggle the box — that only works if they're associated.
    await userEvent.click(screen.getByText('Notify me'))

    expect(screen.getByRole('checkbox', { name: 'Notify me' })).toBeChecked()
  })

  it('toggles on click', async () => {
    const onChange = vi.fn()
    render(<Checkbox label="Done" onChange={onChange} />)

    const checkbox = screen.getByRole('checkbox')
    await userEvent.click(checkbox)

    expect(checkbox).toBeChecked()
    expect(onChange).toHaveBeenCalledOnce()
  })

  it('supports defaultChecked', () => {
    render(<Checkbox label="Done" defaultChecked />)
    expect(screen.getByRole('checkbox')).toBeChecked()
  })

  it('sets the indeterminate DOM property, which has no HTML attribute', () => {
    render(<Checkbox label="Some children done" isIndeterminate />)

    const checkbox = screen.getByRole<HTMLInputElement>('checkbox')
    expect(checkbox.indeterminate).toBe(true)
    expect(checkbox).toBePartiallyChecked()
  })

  it('clears indeterminate when the prop flips', () => {
    const { rerender } = render(<Checkbox label="Parent" isIndeterminate />)
    expect(screen.getByRole<HTMLInputElement>('checkbox').indeterminate).toBe(true)

    rerender(<Checkbox label="Parent" isIndeterminate={false} />)

    expect(screen.getByRole<HTMLInputElement>('checkbox').indeterminate).toBe(false)
  })

  it('does not toggle when disabled', async () => {
    render(<Checkbox label="Done" disabled />)

    const checkbox = screen.getByRole('checkbox')
    await userEvent.click(checkbox)

    expect(checkbox).not.toBeChecked()
  })

  it('marks itself invalid', () => {
    render(<Checkbox label="Accept" isInvalid />)
    expect(screen.getByRole('checkbox')).toHaveAttribute('aria-invalid', 'true')
  })

  it('renders bare when no label is given', () => {
    // Used inside a table row where the header supplies the name.
    render(<Checkbox aria-label="Select row" />)
    expect(screen.getByRole('checkbox', { name: 'Select row' })).toBeInTheDocument()
  })

  it('still forwards a ref while managing indeterminate internally', () => {
    const ref = createRef<HTMLInputElement>()
    render(<Checkbox label="Done" ref={ref} isIndeterminate />)

    expect(ref.current).toBeInstanceOf(HTMLInputElement)
    expect(ref.current?.indeterminate).toBe(true)
  })

  it('supports a callback ref', () => {
    const seen: (HTMLInputElement | null)[] = []
    // Braces matter: React 19 treats a ref callback's return value as a cleanup
    // function, so `ref={(node) => seen.push(node)}` would hand it push()'s number.
    render(
      <Checkbox
        label="Done"
        ref={(node) => {
          seen.push(node)
        }}
      />,
    )

    expect(seen[0]).toBeInstanceOf(HTMLInputElement)
  })
})
