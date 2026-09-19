import { describe, expect, it } from 'vitest'

import { formatMinutes } from './format'

describe('formatMinutes', () => {
  it('breaks minutes into the largest units first, on the working calendar', () => {
    expect(formatMinutes(30)).toBe('30m')
    expect(formatMinutes(60)).toBe('1h')
    expect(formatMinutes(150)).toBe('2h 30m')
    expect(formatMinutes(480)).toBe('1d')
    expect(formatMinutes(500)).toBe('1d 20m')
    expect(formatMinutes(2400)).toBe('1w')
  })

  it('is the inverse of the backend duration grammar round-tripped through a UI label', () => {
    // 1w 2d 3h 4m, in the same 5-day/8-hour week `crate::domain::worklog::parse_duration` uses.
    const totalMinutes = 5 * 8 * 60 + 2 * 8 * 60 + 3 * 60 + 4
    expect(formatMinutes(totalMinutes)).toBe('1w 2d 3h 4m')
  })

  it('renders zero as "0m" rather than an empty string', () => {
    expect(formatMinutes(0)).toBe('0m')
  })
})
