import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Select } from './Select'

const OPTIONS = [
  { label: 'Story', value: 'story' },
  { label: 'Bug', value: 'bug' },
  { label: 'Task', value: 'task' },
]

describe('Select', () => {
  it('associates its label with the control', () => {
    render(<Select label="Card type" options={OPTIONS} />)
    expect(screen.getByLabelText('Card type')).toBeInTheDocument()
  })

  it('renders every option', () => {
    render(<Select label="Card type" options={OPTIONS} />)
    expect(screen.getAllByRole('option')).toHaveLength(3)
  })

  it('selects a value', async () => {
    const onChange = vi.fn()
    render(<Select label="Card type" options={OPTIONS} onChange={onChange} />)

    await userEvent.selectOptions(screen.getByLabelText('Card type'), 'bug')

    expect(screen.getByLabelText('Card type')).toHaveValue('bug')
    expect(onChange).toHaveBeenCalledOnce()
  })

  it('adds a placeholder option when asked', () => {
    render(<Select label="Card type" options={OPTIONS} placeholder="Choose a type" />)

    const options = screen.getAllByRole<HTMLOptionElement>('option')
    expect(options).toHaveLength(4)
    expect(options[0]?.value).toBe('')
    expect(options[0]?.textContent).toBe('Choose a type')
  })

  it('has no placeholder by default', () => {
    render(<Select label="Card type" options={OPTIONS} />)
    expect(screen.getAllByRole('option')).toHaveLength(3)
  })

  it('disables an individual option', () => {
    render(
      <Select
        label="Card type"
        options={[...OPTIONS, { label: 'Epic', value: 'epic', isDisabled: true }]}
      />,
    )

    expect(screen.getByRole('option', { name: 'Epic' })).toBeDisabled()
    expect(screen.getByRole('option', { name: 'Story' })).toBeEnabled()
  })

  it('marks itself invalid and describes the error', () => {
    render(<Select label="Card type" options={OPTIONS} errorMessage="Pick a type" />)

    const select = screen.getByLabelText('Card type')
    expect(select).toHaveAttribute('aria-invalid', 'true')
    expect(select).toHaveAccessibleDescription('Pick a type')
  })

  it('describes itself with the help message', () => {
    render(<Select label="Card type" options={OPTIONS} helpMessage="Types are per project" />)
    expect(screen.getByLabelText('Card type')).toHaveAccessibleDescription('Types are per project')
  })

  it('can be disabled', () => {
    render(<Select label="Card type" options={OPTIONS} disabled />)
    expect(screen.getByLabelText('Card type')).toBeDisabled()
  })

  it('generates unique ids per instance', () => {
    render(
      <>
        <Select label="First" options={OPTIONS} />
        <Select label="Second" options={OPTIONS} />
      </>,
    )

    expect(screen.getByLabelText('First').id).not.toBe(screen.getByLabelText('Second').id)
  })
})
