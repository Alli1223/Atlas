import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { Textarea } from './Textarea'

describe('Textarea', () => {
  it('associates its label with the control', async () => {
    render(<Textarea label="Description" />)

    const textarea = screen.getByLabelText('Description')
    await userEvent.type(textarea, 'Some markdown')

    expect(textarea).toHaveValue('Some markdown')
  })

  it('defaults to 3 rows', () => {
    render(<Textarea label="Description" />)
    expect(screen.getByLabelText('Description')).toHaveAttribute('rows', '3')
  })

  it('accepts a custom row count', () => {
    render(<Textarea label="Description" rows={8} />)
    expect(screen.getByLabelText('Description')).toHaveAttribute('rows', '8')
  })

  it('marks itself invalid and describes the error', () => {
    render(<Textarea label="Description" errorMessage="Too long" />)

    const textarea = screen.getByLabelText('Description')
    expect(textarea).toHaveAttribute('aria-invalid', 'true')
    expect(textarea).toHaveAccessibleDescription('Too long')
  })

  it('describes itself with the help message', () => {
    render(<Textarea label="Description" helpMessage="Markdown supported" />)
    expect(screen.getByLabelText('Description')).toHaveAccessibleDescription('Markdown supported')
  })

  it('can be disabled', () => {
    render(<Textarea label="Description" disabled />)
    expect(screen.getByLabelText('Description')).toBeDisabled()
  })

  it('marks the control required', () => {
    render(<Textarea label="Description" isRequired />)
    expect(screen.getByLabelText(/Description/)).toBeRequired()
  })
})
