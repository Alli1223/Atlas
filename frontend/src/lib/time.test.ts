import { describe, expect, it } from 'vitest'

import { formatBytes, formatMinutes, relativeTime } from './time'

describe('time formatting', () => {
  const now = new Date('2026-11-01T12:00:00Z')

  it('reads recent times relatively', () => {
    expect(relativeTime('2026-11-01T11:59:50Z', now)).toBe('just now')
    expect(relativeTime('2026-11-01T11:55:00Z', now)).toMatch(/5 minutes ago/)
    expect(relativeTime('2026-10-29T12:00:00Z', now)).toMatch(/3 days ago/)
  })

  it('formats durations and sizes', () => {
    expect(formatMinutes(90)).toBe('1h 30m')
    expect(formatMinutes(45)).toBe('45m')
    expect(formatMinutes(120)).toBe('2h')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1536)).toBe('1.5 KB')
  })
})
