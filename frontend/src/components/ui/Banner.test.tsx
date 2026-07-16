import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Banner } from './Banner'
import { Button } from './Button'

describe('Banner', () => {
  it('renders its message', () => {
    render(<Banner>Atlas is in development mode.</Banner>)
    expect(screen.getByText('Atlas is in development mode.')).toBeInTheDocument()
  })

  it('uses role=status for announcements so it waits for a pause', () => {
    render(<Banner>Heads up</Banner>)
    expect(screen.getByRole('status')).toBeInTheDocument()
  })

  it('uses role=status for warnings', () => {
    render(<Banner appearance="warning">Token expires soon</Banner>)
    expect(screen.getByRole('status')).toBeInTheDocument()
  })

  it('uses role=alert for errors so it interrupts', () => {
    // A failed agent run or an expired PAT is worth interrupting for.
    render(<Banner appearance="error">Session failed</Banner>)
    expect(screen.getByRole('alert')).toBeInTheDocument()
  })

  it('renders actions', () => {
    render(
      <Banner appearance="warning" actions={<Button size="compact">Renew</Button>}>
        Token expires in 3 days
      </Banner>,
    )
    expect(screen.getByRole('button', { name: 'Renew' })).toBeInTheDocument()
  })

  it('renders a default icon', () => {
    const { container } = render(<Banner>Message</Banner>)
    expect(container.querySelector('svg')).toBeInTheDocument()
  })

  it('accepts a custom icon', () => {
    render(<Banner icon={<span data-testid="custom" />}>Message</Banner>)
    expect(screen.getByTestId('custom')).toBeInTheDocument()
  })

  it('omits the icon entirely when passed null', () => {
    const { container } = render(<Banner icon={null}>Message</Banner>)
    expect(container.querySelector('svg')).not.toBeInTheDocument()
  })

  it.each(['announcement', 'warning', 'error'] as const)('renders the %s appearance', (appearance) => {
    const { container } = render(<Banner appearance={appearance}>Message</Banner>)
    expect(container.firstElementChild?.className).toContain(appearance)
  })
})
