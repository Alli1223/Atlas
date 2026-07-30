import { describe, expect, it } from 'vitest'

import type { PillStatus } from './api'
import { STATUS_APPEARANCE, STATUS_LABEL, attentionCredentials, needsAttention } from './status'
import { credential } from './test-support'

const ALL_STATUSES: PillStatus[] = ['valid', 'expiring', 'expired', 'invalid', 'unchecked']

describe('status → pill colour', () => {
  // The requested colour contract, asserted at the level of the Lozenge appearance each
  // status resolves to. If the mapping ever collapses (e.g. a refactor points everything at
  // one appearance), these fail rather than shipping a screen where nothing is
  // distinguishable.
  it('maps valid to the green (success) appearance', () => {
    expect(STATUS_APPEARANCE.valid).toBe('success')
  })

  it('maps expiring to the yellow (warning/moved) appearance', () => {
    expect(STATUS_APPEARANCE.expiring).toBe('moved')
  })

  it('maps both expired and invalid to the red (danger/removed) appearance', () => {
    expect(STATUS_APPEARANCE.expired).toBe('removed')
    expect(STATUS_APPEARANCE.invalid).toBe('removed')
  })

  it('maps unchecked to the grey (neutral/default) appearance', () => {
    expect(STATUS_APPEARANCE.unchecked).toBe('default')
  })

  it('separates the calm states from the alarming ones by colour', () => {
    // valid (green) and unchecked (grey) must not share a colour with the failure states —
    // the whole point of the pill is that a bad key looks different at a glance.
    const calm = new Set([STATUS_APPEARANCE.valid, STATUS_APPEARANCE.unchecked])
    const alarming = new Set([
      STATUS_APPEARANCE.expiring,
      STATUS_APPEARANCE.expired,
      STATUS_APPEARANCE.invalid,
    ])
    for (const colour of alarming) {
      expect(calm.has(colour)).toBe(false)
    }
  })

  it('gives every status a label', () => {
    for (const status of ALL_STATUSES) {
      expect(STATUS_LABEL[status]).toBeTruthy()
    }
  })
})

describe('needsAttention', () => {
  it('flags expiring, expired and invalid', () => {
    expect(needsAttention('expiring')).toBe(true)
    expect(needsAttention('expired')).toBe(true)
    expect(needsAttention('invalid')).toBe(true)
  })

  it('does NOT flag valid or unchecked', () => {
    // The guard the warning banner depends on. If needsAttention regressed to `true`
    // for everything, a healthy instance would nag on every screen — these two assertions
    // are what fail first.
    expect(needsAttention('valid')).toBe(false)
    expect(needsAttention('unchecked')).toBe(false)
  })
})

describe('attentionCredentials', () => {
  it('returns only the credentials that need attention, in order', () => {
    const rows = [
      credential({ label: 'ok', status: 'valid' }),
      credential({ label: 'dying', status: 'expiring' }),
      credential({ label: 'fresh', status: 'unchecked' }),
      credential({ label: 'dead', status: 'expired' }),
      credential({ label: 'rejected', status: 'invalid' }),
    ]
    expect(attentionCredentials(rows).map((c) => c.label)).toEqual(['dying', 'dead', 'rejected'])
  })

  it('returns nothing when every key is healthy or merely unchecked', () => {
    const rows = [
      credential({ status: 'valid' }),
      credential({ status: 'unchecked' }),
    ]
    expect(attentionCredentials(rows)).toEqual([])
  })
})
