import { render } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Skeleton, SkeletonText } from './Skeleton'

describe('Skeleton', () => {
  it('is hidden from assistive tech', () => {
    // The surrounding region owns the busy state; otherwise a loading board becomes a
    // wall of announcements.
    const { container } = render(<Skeleton />)
    expect(container.firstElementChild).toHaveAttribute('aria-hidden', 'true')
  })

  it('applies width and height', () => {
    const { container } = render(<Skeleton width={120} height={16} />)

    const el = container.firstElementChild as HTMLElement
    expect(el.style.width).toBe('120px')
    expect(el.style.height).toBe('16px')
  })

  it('accepts CSS length strings', () => {
    const { container } = render(<Skeleton width="60%" height="2rem" />)

    const el = container.firstElementChild as HTMLElement
    expect(el.style.width).toBe('60%')
    expect(el.style.height).toBe('2rem')
  })

  it('shimmers by default', () => {
    const { container } = render(<Skeleton />)
    expect(container.firstElementChild?.className).toContain('shimmer')
  })

  it('can drop the shimmer', () => {
    const { container } = render(<Skeleton hasShimmer={false} />)
    expect(container.firstElementChild?.className).not.toContain('shimmer')
  })

  it('renders a circle for avatar placeholders', () => {
    const { container } = render(<Skeleton isCircle />)
    expect(container.firstElementChild?.className).toContain('circle')
  })
})

describe('SkeletonText', () => {
  it('renders three lines by default', () => {
    const { container } = render(<SkeletonText />)
    expect(container.querySelectorAll('span > span')).toHaveLength(3)
  })

  it('renders the requested number of lines', () => {
    const { container } = render(<SkeletonText lines={5} />)
    expect(container.querySelectorAll('span > span')).toHaveLength(5)
  })

  it('shortens the last line, which is what makes it read as text', () => {
    const { container } = render(<SkeletonText lines={3} />)

    const lines = [...container.querySelectorAll<HTMLElement>('span > span')]
    expect(lines[0]?.style.width).toBe('100%')
    expect(lines[2]?.style.width).toBe('60%')
  })

  it('keeps a single line full width', () => {
    const { container } = render(<SkeletonText lines={1} />)

    const lines = [...container.querySelectorAll<HTMLElement>('span > span')]
    expect(lines[0]?.style.width).toBe('100%')
  })

  it('is hidden from assistive tech', () => {
    const { container } = render(<SkeletonText />)
    expect(container.firstElementChild).toHaveAttribute('aria-hidden', 'true')
  })
})
