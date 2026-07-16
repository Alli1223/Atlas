import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Input } from './Input'

describe('Input', () => {
  it('associates its label with the control', async () => {
    render(<Input label="Summary" />)

    // getByLabelText only resolves if htmlFor/id are actually wired up.
    const input = screen.getByLabelText('Summary')
    await userEvent.type(input, 'Fix the board')

    expect(input).toHaveValue('Fix the board')
  })

  it('generates a unique id per instance', () => {
    render(
      <>
        <Input label="First" />
        <Input label="Second" />
      </>,
    )

    const first = screen.getByLabelText('First')
    const second = screen.getByLabelText('Second')

    expect(first.id).not.toBe(second.id)
    expect(first.id).toBeTruthy()
  })

  it('respects an explicit id', () => {
    render(<Input label="Summary" id="summary-field" />)
    expect(screen.getByLabelText('Summary')).toHaveAttribute('id', 'summary-field')
  })

  it('describes itself with the help message', () => {
    render(<Input label="Key" helpMessage="Keys are permanent" />)
    expect(screen.getByLabelText('Key')).toHaveAccessibleDescription('Keys are permanent')
  })

  it('marks itself invalid and points at the error', () => {
    render(<Input label="Summary" errorMessage="Summary is required" />)

    const input = screen.getByLabelText('Summary')
    expect(input).toHaveAttribute('aria-invalid', 'true')
    expect(input).toHaveAccessibleDescription('Summary is required')
  })

  it('shows the error instead of the help message, never both', () => {
    render(<Input label="Summary" helpMessage="Some hint" errorMessage="Required" />)

    expect(screen.getByText('Required')).toBeInTheDocument()
    expect(screen.queryByText('Some hint')).not.toBeInTheDocument()
    expect(screen.getByLabelText('Summary')).toHaveAccessibleDescription('Required')
  })

  it('supports isInvalid without an error message', () => {
    render(<Input label="Summary" isInvalid />)
    expect(screen.getByLabelText('Summary')).toHaveAttribute('aria-invalid', 'true')
  })

  it('is not marked invalid by default', () => {
    render(<Input label="Summary" />)
    expect(screen.getByLabelText('Summary')).not.toHaveAttribute('aria-invalid')
  })

  it('preserves a caller-supplied aria-describedby', () => {
    render(<Input label="Summary" aria-describedby="external" errorMessage="Required" />)

    const describedBy = screen.getByLabelText('Summary').getAttribute('aria-describedby')
    expect(describedBy).toContain('external')
  })

  it('does not accept input when disabled', async () => {
    const onChange = vi.fn()
    render(<Input label="Summary" disabled onChange={onChange} />)

    await userEvent.type(screen.getByLabelText('Summary'), 'nope')

    expect(onChange).not.toHaveBeenCalled()
  })

  it('marks the control required', () => {
    render(<Input label="Summary" isRequired />)
    expect(screen.getByLabelText(/Summary/)).toBeRequired()
  })
})
