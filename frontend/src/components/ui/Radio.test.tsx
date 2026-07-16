import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Radio, RadioGroup } from './Radio'

describe('Radio', () => {
  it('associates the label with the control', async () => {
    render(<Radio name="t" value="a" label="Option A" />)

    await userEvent.click(screen.getByText('Option A'))

    expect(screen.getByRole('radio', { name: 'Option A' })).toBeChecked()
  })

  it('marks itself invalid', () => {
    render(<Radio name="t" value="a" label="A" isInvalid />)
    expect(screen.getByRole('radio')).toHaveAttribute('aria-invalid', 'true')
  })

  it('renders bare without a label', () => {
    render(<Radio name="t" value="a" aria-label="Bare" />)
    expect(screen.getByRole('radio', { name: 'Bare' })).toBeInTheDocument()
  })
})

describe('RadioGroup', () => {
  const OPTIONS = [
    { label: 'Story points', value: 'points' },
    { label: 'Hours', value: 'hours' },
    { label: 'None', value: 'none' },
  ]

  it('groups the radios under its legend', () => {
    render(<RadioGroup label="Estimation" name="est" options={OPTIONS} />)

    // A fieldset/legend is what gives the set a single accessible group name.
    expect(screen.getByRole('group', { name: 'Estimation' })).toBeInTheDocument()
    expect(screen.getAllByRole('radio')).toHaveLength(3)
  })

  it('reports the chosen value', async () => {
    const onChange = vi.fn()
    render(<RadioGroup label="Estimation" name="est" options={OPTIONS} onChange={onChange} />)

    await userEvent.click(screen.getByRole('radio', { name: 'Hours' }))

    expect(onChange).toHaveBeenCalledWith('hours')
  })

  it('honours defaultValue when uncontrolled', () => {
    render(<RadioGroup label="Estimation" name="est" options={OPTIONS} defaultValue="none" />)
    expect(screen.getByRole('radio', { name: 'None' })).toBeChecked()
  })

  it('reflects the value when controlled', () => {
    const { rerender } = render(
      <RadioGroup label="Estimation" name="est" options={OPTIONS} value="points" />,
    )
    expect(screen.getByRole('radio', { name: 'Story points' })).toBeChecked()

    rerender(<RadioGroup label="Estimation" name="est" options={OPTIONS} value="hours" />)

    expect(screen.getByRole('radio', { name: 'Hours' })).toBeChecked()
    expect(screen.getByRole('radio', { name: 'Story points' })).not.toBeChecked()
  })

  it('shares one name so the radios are mutually exclusive', () => {
    render(<RadioGroup label="Estimation" name="est" options={OPTIONS} />)

    for (const radio of screen.getAllByRole<HTMLInputElement>('radio')) {
      expect(radio.name).toBe('est')
    }
  })

  it('disables a single option', () => {
    render(
      <RadioGroup
        label="Estimation"
        name="est"
        options={[...OPTIONS, { label: 'T-shirt', value: 'tshirt', isDisabled: true }]}
      />,
    )

    expect(screen.getByRole('radio', { name: 'T-shirt' })).toBeDisabled()
    expect(screen.getByRole('radio', { name: 'Hours' })).toBeEnabled()
  })

  it('disables every option when the group is disabled', () => {
    render(<RadioGroup label="Estimation" name="est" options={OPTIONS} isDisabled />)

    for (const radio of screen.getAllByRole('radio')) {
      expect(radio).toBeDisabled()
    }
  })
})
