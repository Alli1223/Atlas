import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { Lozenge, STATUS_CATEGORY_APPEARANCE } from './Lozenge'

describe('Lozenge', () => {
  it('renders its text', () => {
    render(<Lozenge>In Progress</Lozenge>)
    expect(screen.getByText('In Progress')).toBeInTheDocument()
  })

  describe('status categories', () => {
    // The mapping Jira itself uses. To Do = grey, In Progress = blue, Done = lime.
    // If these ever flip, every board in Atlas lies about its state.
    it('maps the three Atlas status categories the way ADS does', () => {
      expect(STATUS_CATEGORY_APPEARANCE).toEqual({
        todo: 'default',
        inprogress: 'inprogress',
        done: 'success',
      })
    })

    it.each([
      ['todo', 'default'],
      ['inprogress', 'inprogress'],
      ['done', 'success'],
    ] as const)('renders %s with the %s appearance class', (category, appearance) => {
      const { container } = render(<Lozenge statusCategory={category}>Status</Lozenge>)
      // CSS modules hash class names, so assert on the readable stem rather than equality.
      expect(container.firstElementChild?.className).toContain(appearance)
    })

    it('lets statusCategory win over appearance', () => {
      const { container } = render(
        <Lozenge appearance="removed" statusCategory="done">
          Done
        </Lozenge>,
      )
      expect(container.firstElementChild?.className).toContain('success')
      expect(container.firstElementChild?.className).not.toContain('removed')
    })
  })

  it.each(['default', 'inprogress', 'success', 'removed', 'new', 'moved'] as const)(
    'renders the %s appearance',
    (appearance) => {
      const { container } = render(<Lozenge appearance={appearance}>Label</Lozenge>)
      expect(container.firstElementChild?.className).toContain(appearance)
    },
  )

  it('applies the bold variant', () => {
    const { container } = render(<Lozenge isBold>Done</Lozenge>)
    expect(container.firstElementChild?.className).toContain('bold')
  })

  it('is subtle by default', () => {
    const { container } = render(<Lozenge>Done</Lozenge>)
    expect(container.firstElementChild?.className).not.toContain('bold')
  })

  it('passes through a custom className', () => {
    const { container } = render(<Lozenge className="mine">Done</Lozenge>)
    expect(container.firstElementChild?.className).toContain('mine')
  })
})
