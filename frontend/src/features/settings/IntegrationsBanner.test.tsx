import { screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { IntegrationsBanner } from './IntegrationsBanner'
import { credential, renderWithCredentials } from './test-support'

describe('IntegrationsBanner', () => {
  it('renders nothing when every key is valid or unchecked', () => {
    // The load-bearing case: a healthy instance must be silent. If the banner regressed to
    // always-on, this is the assertion that fails.
    const { container } = renderWithCredentials(<IntegrationsBanner />, [
      credential({ status: 'valid' }),
      credential({ status: 'unchecked' }),
    ])
    expect(container).toBeEmptyDOMElement()
  })

  it('renders nothing for an instance with no keys at all', () => {
    const { container } = renderWithCredentials(<IntegrationsBanner />, [])
    expect(container).toBeEmptyDOMElement()
  })

  it('warns (role=status) when the worst key is only expiring', () => {
    renderWithCredentials(<IntegrationsBanner />, [
      credential({ status: 'valid' }),
      credential({ status: 'expiring' }),
    ])
    const banner = screen.getByRole('status')
    expect(banner).toHaveAttribute('data-appearance', 'warning')
  })

  it('alerts (role=alert) when a key is expired', () => {
    renderWithCredentials(<IntegrationsBanner />, [credential({ status: 'expired' })])
    const banner = screen.getByRole('alert')
    expect(banner).toHaveAttribute('data-appearance', 'error')
  })

  it('alerts when a key is invalid', () => {
    renderWithCredentials(<IntegrationsBanner />, [credential({ status: 'invalid' })])
    expect(screen.getByRole('alert')).toHaveAttribute('data-appearance', 'error')
  })

  it('names the single offending provider and key', () => {
    renderWithCredentials(<IntegrationsBanner />, [
      credential({ provider: 'github', label: 'work laptop', status: 'expired' }),
    ])
    expect(screen.getByText(/GitHub key .*work laptop.* has expired/i)).toBeInTheDocument()
  })

  it('summarises the count when several keys need attention', () => {
    renderWithCredentials(<IntegrationsBanner />, [
      credential({ status: 'expired' }),
      credential({ status: 'invalid' }),
      credential({ status: 'expiring' }),
    ])
    expect(screen.getByText(/3 integration keys need attention/i)).toBeInTheDocument()
  })
})
