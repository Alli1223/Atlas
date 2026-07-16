import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Tag, TAG_COLORS } from './Tag'

describe('Tag', () => {
  it('renders its text', () => {
    render(<Tag>tech-debt</Tag>)
    expect(screen.getByText('tech-debt')).toBeInTheDocument()
  })

  it.each(TAG_COLORS)('renders the %s colour', (color) => {
    const { container } = render(<Tag color={color}>label</Tag>)
    expect(container.firstElementChild?.className).toContain(color)
  })

  it('renders as a link when href is given', () => {
    render(<Tag href="/board?tags=bug">bug</Tag>)
    expect(screen.getByRole('link', { name: 'bug' })).toHaveAttribute('href', '/board?tags=bug')
  })

  it('is not a link by default', () => {
    render(<Tag>bug</Tag>)
    expect(screen.queryByRole('link')).not.toBeInTheDocument()
  })

  describe('removable', () => {
    it('has no remove button unless onRemove is given', () => {
      render(<Tag>bug</Tag>)
      expect(screen.queryByRole('button')).not.toBeInTheDocument()
    })

    it('calls onRemove when clicked', async () => {
      const onRemove = vi.fn()
      render(<Tag onRemove={onRemove}>bug</Tag>)

      await userEvent.click(screen.getByRole('button', { name: 'Remove bug' }))

      expect(onRemove).toHaveBeenCalledOnce()
    })

    it('names the remove button after the tag so a screen reader knows which one', () => {
      render(<Tag onRemove={vi.fn()}>needs-review</Tag>)
      expect(screen.getByRole('button', { name: 'Remove needs-review' })).toBeInTheDocument()
    })

    it('falls back to a generic label for non-string children', () => {
      render(
        <Tag onRemove={vi.fn()}>
          <em>bug</em>
        </Tag>,
      )
      expect(screen.getByRole('button', { name: 'Remove tag' })).toBeInTheDocument()
    })

    it('accepts an explicit remove label', () => {
      render(
        <Tag onRemove={vi.fn()} removeButtonLabel="Remove the bug label">
          bug
        </Tag>,
      )
      expect(screen.getByRole('button', { name: 'Remove the bug label' })).toBeInTheDocument()
    })

    it('is keyboard reachable', async () => {
      const onRemove = vi.fn()
      render(<Tag onRemove={onRemove}>bug</Tag>)

      await userEvent.tab()
      await userEvent.keyboard('{Enter}')

      expect(onRemove).toHaveBeenCalledOnce()
    })
  })

  it('applies the rounded variant', () => {
    const { container } = render(<Tag isRounded>bug</Tag>)
    expect(container.firstElementChild?.className).toContain('rounded')
  })
})
