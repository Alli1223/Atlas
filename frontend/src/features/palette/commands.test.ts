import { describe, expect, it } from 'vitest'

import { commandsFor } from './commands'

describe('commandsFor', () => {
  const projects = [{ key: 'JOB', name: 'Job hunt' }]

  it('opens a card key first', () => {
    expect(commandsFor('job-12', projects)[0]).toMatchObject({ label: 'Open JOB-12', params: { key: 'JOB-12' } })
  })

  it('filters by label and offers a text search last', () => {
    const found = commandsFor('reports', projects)
    expect(found.map((c) => c.id)).toEqual(['JOB-reports', 'search-text'])
    expect(found.at(-1)?.search).toEqual({ q: 'text ~ "reports"' })
  })

  it('lists everything for an empty query', () => {
    expect(commandsFor('', projects).length).toBe(10)
  })
})
