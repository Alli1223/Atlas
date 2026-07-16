import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Button } from './Button'
import { EmptyState } from './EmptyState'

describe('EmptyState', () => {
  it('renders its header as a heading', () => {
    render(<EmptyState header="No cards match this filter" />)
    expect(screen.getByRole('heading', { name: 'No cards match this filter' })).toBeInTheDocument()
  })

  it('renders the description', () => {
    render(<EmptyState header="Nothing here" description="Try clearing a quick filter." />)
    expect(screen.getByText('Try clearing a quick filter.')).toBeInTheDocument()
  })

  it('omits the description when not given', () => {
    const { container } = render(<EmptyState header="Nothing here" />)
    expect(container.querySelector('p')).not.toBeInTheDocument()
  })

  it('renders both actions', () => {
    render(
      <EmptyState
        header="No cards"
        primaryAction={<Button appearance="primary">Create card</Button>}
        secondaryAction={<Button appearance="subtle">Clear filters</Button>}
      />,
    )

    expect(screen.getByRole('button', { name: 'Create card' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Clear filters' })).toBeInTheDocument()
  })

  it('hides the illustration from assistive tech', () => {
    // The header carries the meaning; the image is decoration.
    const { container } = render(
      <EmptyState header="Nothing here" image={<svg data-testid="art" />} />,
    )

    expect(container.querySelector('[aria-hidden="true"]')).toBeInTheDocument()
    expect(screen.getByTestId('art')).toBeInTheDocument()
  })

  it('applies the compact variant', () => {
    const { container } = render(<EmptyState header="Nothing here" isCompact />)
    expect(container.firstElementChild?.className).toContain('narrow')
  })
})
