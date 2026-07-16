import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Spinner } from './Spinner'

describe('Spinner', () => {
  it('announces itself as a status by default', () => {
    render(<Spinner />)
    expect(screen.getByRole('status', { name: 'Loading' })).toBeInTheDocument()
  })

  it('accepts a custom label', () => {
    render(<Spinner label="Starting agent session" />)
    expect(screen.getByRole('status', { name: 'Starting agent session' })).toBeInTheDocument()
  })

  it('goes silent when label is null', () => {
    // For use inside a control that already carries aria-busy — otherwise the state is
    // announced twice.
    render(<Spinner label={null} />)
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it.each([
    ['xsmall', 12],
    ['small', 16],
    ['medium', 24],
    ['large', 48],
    ['xlarge', 96],
  ] as const)('renders %s at %spx', (size, px) => {
    const { container } = render(<Spinner size={size} />)

    const svg = container.querySelector('svg')
    expect(svg).toHaveAttribute('width', String(px))
    expect(svg).toHaveAttribute('height', String(px))
  })

  it('opts out of the global reduced-motion freeze', () => {
    // A spinner frozen at 1ms stops communicating "busy" — that is a regression, not an
    // accommodation, so it carries the escape hatch attribute.
    const { container } = render(<Spinner />)
    expect(container.querySelector('svg')).toHaveAttribute('data-preserve-motion')
  })
})
