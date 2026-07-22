import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import type { ChildRollup } from './api'
import { CardMiniMap, miniBoardBlocks } from './CardMiniMap'

/** The block count rendered in one mini-column, read from its `data-*` markers and children. */
function column(container: HTMLElement, category: 'todo' | 'inProgress' | 'done') {
  const el = container.querySelector<HTMLElement>(`[data-category="${category}"]`)
  if (!el) throw new Error(`no ${category} column`)
  return {
    declared: Number(el.dataset.blocks),
    rendered: el.querySelectorAll(':scope > span').length,
  }
}

describe('miniBoardBlocks', () => {
  it('scales the busiest category to the max and the rest proportionally', () => {
    // peak = 3 (todo). todo → 5, inProgress 2/3·5 ≈ 3, done 2/3·5 ≈ 3.
    expect(miniBoardBlocks({ total: 7, todo: 3, inProgress: 2, done: 2 })).toEqual({
      todo: 5,
      inProgress: 3,
      done: 3,
    })
  })

  it('draws nothing for an empty category and floors a non-empty one at one block', () => {
    // A single in-progress card against six to-do must still show one block, not vanish.
    expect(miniBoardBlocks({ total: 7, todo: 6, inProgress: 1, done: 0 })).toEqual({
      todo: 5,
      inProgress: 1,
      done: 0,
    })
  })

  it('reads as all-done when every child is done', () => {
    expect(miniBoardBlocks({ total: 4, todo: 0, inProgress: 0, done: 4 })).toEqual({
      todo: 0,
      inProgress: 0,
      done: 5,
    })
  })
})

describe('CardMiniMap', () => {
  const rollup: ChildRollup = { total: 7, todo: 3, inProgress: 2, done: 2 }

  it('renders one mini-column per category with blocks matching the rollup', () => {
    const { container } = render(<CardMiniMap rollup={rollup} />)

    // The blocks the DOM draws must equal what the pure scaler says — a regression in either
    // the render or the scaler breaks this, and the mini-map would misreport the child board.
    const expected = miniBoardBlocks(rollup)
    for (const category of ['todo', 'inProgress', 'done'] as const) {
      const { declared, rendered } = column(container, category)
      expect(declared).toBe(expected[category])
      expect(rendered).toBe(expected[category])
    }
  })

  it('labels the progress and announces the child distribution', () => {
    render(<CardMiniMap rollup={rollup} />)

    expect(screen.getByText('2/7 done')).toBeInTheDocument()
    expect(screen.getByLabelText('2 of 7 child cards done')).toBeInTheDocument()
    expect(
      screen.getByLabelText('Child board: 3 to do, 2 in progress, 2 done'),
    ).toBeInTheDocument()
  })

  it('draws a full done column and empty others when work is finished', () => {
    const { container } = render(
      <CardMiniMap rollup={{ total: 4, todo: 0, inProgress: 0, done: 4 }} />,
    )
    expect(column(container, 'todo').rendered).toBe(0)
    expect(column(container, 'inProgress').rendered).toBe(0)
    expect(column(container, 'done').rendered).toBe(5)
  })
})
