import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Avatar, avatarColor, AvatarGroup, initials } from './Avatar'

describe('initials', () => {
  it.each([
    ['Alastair Rayner', 'AR'],
    ['ada lovelace', 'AL'],
    ['Grace', 'G'],
    ['  Katherine   Johnson  ', 'KJ'],
    // Middle names are dropped rather than crowding a 16px circle.
    ['John Ronald Reuel Tolkien', 'JT'],
  ])('turns %s into %s', (name, expected) => {
    expect(initials(name)).toBe(expected)
  })

  it('survives an empty name', () => {
    expect(initials('')).toBe('')
  })
})

describe('avatarColor', () => {
  it('is stable for a given name', () => {
    // A person must keep the same colour across every board and reload, or the avatar
    // stops being a recognition aid.
    expect(avatarColor('Alastair Rayner')).toBe(avatarColor('Alastair Rayner'))
  })

  it('is deterministic across long names', () => {
    const name = 'A very long display name that goes on and on'
    expect(avatarColor(name)).toBe(avatarColor(name))
  })

  it('spreads names across the palette', () => {
    const names = ['Ada', 'Grace', 'Alan', 'Katherine', 'Linus', 'Margaret', 'Barbara']
    const colors = new Set(names.map(avatarColor))
    expect(colors.size).toBeGreaterThan(1)
  })
})

describe('Avatar', () => {
  it('exposes the name to assistive tech', () => {
    render(<Avatar name="Ada Lovelace" />)
    expect(screen.getByRole('img', { name: 'Ada Lovelace' })).toBeInTheDocument()
  })

  it('shows initials when there is no image', () => {
    render(<Avatar name="Ada Lovelace" />)
    expect(screen.getByText('AL')).toBeInTheDocument()
  })

  it('renders an image when src is given', () => {
    render(<Avatar name="Ada Lovelace" src="/ada.png" />)

    const img = screen.getByRole('img', { name: 'Ada Lovelace' }).querySelector('img')
    expect(img).toHaveAttribute('src', '/ada.png')
    // The wrapper carries the name; a duplicated alt would announce it twice.
    expect(img).toHaveAttribute('alt', '')
  })

  it('falls back to initials when the image fails to load', () => {
    render(<Avatar name="Ada Lovelace" src="/broken.png" />)

    const img = screen.getByRole('img', { name: 'Ada Lovelace' }).querySelector('img')
    expect(img).not.toBeNull()
    fireEvent.error(img!)

    // A torn-image icon on a board card is worse than initials.
    expect(screen.getByText('AL')).toBeInTheDocument()
  })

  it('becomes a button when clickable', async () => {
    const onClick = vi.fn()
    render(<Avatar name="Ada Lovelace" onClick={onClick} />)

    await userEvent.click(screen.getByRole('button', { name: 'Ada Lovelace' }))

    expect(onClick).toHaveBeenCalledOnce()
  })

  it.each(['xsmall', 'small', 'medium', 'large', 'xlarge', 'xxlarge'] as const)(
    'renders size %s',
    (size) => {
      const { container } = render(<Avatar name="Ada" size={size} />)
      expect(container.firstElementChild?.className).toContain(size)
    },
  )

  it('renders the square appearance for non-person avatars', () => {
    const { container } = render(<Avatar name="Atlas" appearance="square" />)
    expect(container.firstElementChild?.className).toContain('square')
  })
})

describe('AvatarGroup', () => {
  it('renders its children', () => {
    render(
      <AvatarGroup>
        <Avatar name="Ada Lovelace" isStacked />
        <Avatar name="Grace Hopper" isStacked />
      </AvatarGroup>,
    )

    expect(screen.getAllByRole('img')).toHaveLength(2)
  })
})
