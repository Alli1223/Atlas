import { describe, expect, it } from 'vitest'

import { foldSeries, seriesColour } from './charts'

describe('chart series', () => {
  it('folds past seven series into Other instead of cycling hues', () => {
    const series = Array.from({ length: 9 }, (_, i) => ({ id: String(i), name: `S${String(i)}`, values: [1, i] }))
    const folded = foldSeries(series)
    expect(folded).toHaveLength(8)
    expect(folded[7]).toMatchObject({ id: 'other', values: [2, 15] })
    expect(seriesColour(8)).toBe('var(--series-8)')
  })
})
