import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import type { Tag } from './api'
import { TagList } from './TagList'

function tag(name: string, overrides: Partial<Tag> = {}): Tag {
  return {
    id: `id-${name}`,
    projectId: 'project-1',
    name,
    colour: 'blue',
    createdAt: '2026-07-17T05:30:55.274014Z',
    ...overrides,
  }
}

describe('TagList', () => {
  it('renders a chip per tag', () => {
    render(<TagList tags={[tag('bug'), tag('hotfix')]} />)
    expect(screen.getByText('bug')).toBeInTheDocument()
    expect(screen.getByText('hotfix')).toBeInTheDocument()
  })

  it('is a list, so a screen reader announces the tags as a set', () => {
    // Four unrelated spans read as the sentence "bug hotfix blocked needs-review".
    // A list reads as "list, 2 items" — which is what the coloured chips convey visually.
    render(<TagList tags={[tag('bug'), tag('hotfix')]} label="Card tags" />)
    const list = screen.getByRole('list', { name: 'Card tags' })
    expect(within(list).getAllByRole('listitem')).toHaveLength(2)
  })

  it('renders every tag name as text, never colour alone', () => {
    // WCAG 1.4.1: colour groups related tags at a glance, but it is decoration. Anyone who
    // has not memorised what teal means on this board needs the word.
    render(<TagList tags={[tag('security', { colour: 'red' })]} />)
    expect(screen.getByText('security')).toBeVisible()
  })

  it('applies the tag colour to the chip', () => {
    const { container } = render(<TagList tags={[tag('offer', { colour: 'green' })]} />)
    expect(container.querySelector('[class*="green"]')).not.toBeNull()
  })

  it('falls back to the neutral chip when the server sent no colour', () => {
    // `colour` is nullable server-side and means "no colour chosen". The primitive spells
    // that `standard`; passing `null` straight through would render an unstyled chip.
    const { container } = render(<TagList tags={[tag('plain', { colour: null })]} />)
    expect(container.querySelector('[class*="standard"]')).not.toBeNull()
  })

  it('has no remove buttons unless onRemove is given', () => {
    render(<TagList tags={[tag('bug')]} />)
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('removes the tag it was clicked on, not merely "a tag"', async () => {
    const onRemove = vi.fn()
    const tags = [tag('bug'), tag('hotfix')]
    render(<TagList tags={tags} onRemove={onRemove} />)

    await userEvent.click(screen.getByRole('button', { name: 'Remove tag hotfix' }))

    expect(onRemove).toHaveBeenCalledExactlyOnceWith(tags[1])
  })

  it('names each remove button after its tag', () => {
    // "Remove" x4 in the a11y tree is a list nobody can navigate.
    render(<TagList tags={[tag('bug'), tag('hotfix')]} onRemove={vi.fn()} />)
    expect(screen.getByRole('button', { name: 'Remove tag bug' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Remove tag hotfix' })).toBeInTheDocument()
  })

  it('links each chip when hrefForTag is given', () => {
    render(<TagList tags={[tag('bug')]} hrefForTag={(t) => `/board?tags=${t.name}`} />)
    expect(screen.getByRole('link', { name: 'bug' })).toHaveAttribute('href', '/board?tags=bug')
  })

  describe('when empty', () => {
    it('renders nothing at all by default', () => {
      const { container } = render(<TagList tags={[]} />)
      expect(container).toBeEmptyDOMElement()
    })

    it('renders the empty message when one is given', () => {
      render(<TagList tags={[]} emptyMessage="No tags yet" />)
      expect(screen.getByText('No tags yet')).toBeInTheDocument()
    })

    it('renders no list when empty, so a screen reader is not told about an empty list', () => {
      render(<TagList tags={[]} emptyMessage="No tags yet" />)
      expect(screen.queryByRole('list')).not.toBeInTheDocument()
    })
  })
})
