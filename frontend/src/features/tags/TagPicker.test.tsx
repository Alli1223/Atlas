import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import type { Tag, TagUsage } from './api'
import { TagPicker } from './TagPicker'

function usage(name: string, usageCount = 0, overrides: Partial<TagUsage> = {}): TagUsage {
  return {
    id: `id-${name}`,
    projectId: 'project-1',
    name,
    colour: 'blue',
    createdAt: '2026-07-17T05:30:55.274014Z',
    usageCount,
    ...overrides,
  }
}

/** The General preset list, which is what a blank project's picker actually shows. */
const OPTIONS: TagUsage[] = [
  usage('admin'),
  usage('blocked', 3),
  usage('idea'),
  usage('question'),
  usage('research', 1),
  usage('urgent', 12),
  usage('waiting'),
]

function setup(props: Partial<React.ComponentProps<typeof TagPicker>> = {}) {
  const onSelect = vi.fn()
  const onDeselect = vi.fn()
  const onCreate = vi.fn()

  const result = render(
    <TagPicker
      options={OPTIONS}
      selected={[]}
      onSelect={onSelect}
      onDeselect={onDeselect}
      onCreate={onCreate}
      {...props}
    />,
  )

  return { ...result, onSelect, onDeselect, onCreate }
}

const combobox = () => screen.getByRole('combobox')

/**
 * The tag rows, with the create row excluded.
 *
 * The create row is an `option` too — it has to be, so one arrow-key handler serves both
 * and `aria-activedescendant` can point at it. So a test about *filtering* has to say
 * which rows it means.
 */
function tagOptionNames(): string[] {
  return within(screen.getByRole('listbox'))
    .getAllByRole('option')
    .map((o) => o.textContent ?? '')
    .filter((text) => !text.startsWith('Create'))
}

describe('TagPicker', () => {
  describe('the combobox contract', () => {
    it('is closed until focused', () => {
      setup()
      expect(combobox()).toHaveAttribute('aria-expanded', 'false')
      expect(screen.queryByRole('listbox')).not.toBeInTheDocument()
    })

    it('opens on focus and exposes its options', async () => {
      setup()
      await userEvent.click(combobox())

      expect(combobox()).toHaveAttribute('aria-expanded', 'true')
      expect(within(screen.getByRole('listbox')).getAllByRole('option')).toHaveLength(
        OPTIONS.length,
      )
    })

    it('points aria-activedescendant at the highlighted row', async () => {
      // The whole reason focus can stay in the input while an option is highlighted. Get
      // this wrong and a screen-reader user hears nothing as they arrow through the list.
      setup()
      await userEvent.click(combobox())
      await userEvent.keyboard('{ArrowDown}')

      const active = combobox().getAttribute('aria-activedescendant')
      expect(active).not.toBeNull()
      expect(document.getElementById(active!)).toHaveAttribute('role', 'option')
    })

    it('closes on Escape', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.keyboard('{Escape}')

      expect(combobox()).toHaveAttribute('aria-expanded', 'false')
    })
  })

  describe('autocomplete', () => {
    it('filters to matching tags as you type', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'que')

      expect(tagOptionNames()).toEqual([expect.stringContaining('question')])
    })

    it('ranks prefix matches first', async () => {
      setup({ options: [usage('breaking-change'), usage('reference'), usage('refactor')] })
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 're')

      // `breaking-change` contains "re" but is not what anyone typing "re" meant.
      const names = tagOptionNames()
      expect(names.at(-1)).toContain('breaking-change')
      expect(names.slice(0, 2).join(' ')).toContain('refactor')
    })

    it('shows the usage count, which is why the list is worth reading', async () => {
      // The number says which tags this board actually uses, so the list sorts itself in
      // the user's head.
      setup()
      await userEvent.click(combobox())

      const urgent = screen.getByRole('option', { name: /urgent/ })
      expect(urgent).toHaveTextContent('12')
    })

    it('says so when nothing matches and creation is off', async () => {
      setup({ canCreate: false })
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'zzzz')

      expect(screen.getByText('No matching tags')).toBeInTheDocument()
    })
  })

  describe('selecting', () => {
    it('selects the clicked tag', async () => {
      const { onSelect } = setup()
      await userEvent.click(combobox())
      await userEvent.click(screen.getByRole('option', { name: /blocked/ }))

      expect(onSelect).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({ name: 'blocked' }),
      )
    })

    it('selects with Enter on the highlighted row', async () => {
      const { onSelect } = setup()
      await userEvent.click(combobox())
      await userEvent.keyboard('{ArrowDown}{Enter}')

      expect(onSelect).toHaveBeenCalledOnce()
    })

    it('marks already-selected tags with aria-selected', async () => {
      setup({ selected: [OPTIONS[1] as Tag] })
      await userEvent.click(combobox())

      expect(screen.getByRole('option', { name: /blocked/ })).toHaveAttribute(
        'aria-selected',
        'true',
      )
      expect(screen.getByRole('option', { name: /urgent/ })).toHaveAttribute(
        'aria-selected',
        'false',
      )
    })

    it('deselects a tag that is picked again', async () => {
      const { onDeselect, onSelect } = setup({ selected: [OPTIONS[1] as Tag] })
      await userEvent.click(combobox())
      await userEvent.click(screen.getByRole('option', { name: /blocked/ }))

      expect(onDeselect).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({ name: 'blocked' }),
      )
      expect(onSelect).not.toHaveBeenCalled()
    })

    it('stays open after a pick, because adding three tags is the common case', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.click(screen.getByRole('option', { name: /blocked/ }))

      expect(combobox()).toHaveAttribute('aria-expanded', 'true')
    })

    it('clears the query after a pick', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'blo')
      await userEvent.click(screen.getByRole('option', { name: /blocked/ }))

      expect(combobox()).toHaveValue('')
    })

    it('renders the selected tags as dismissable chips', async () => {
      const { onDeselect } = setup({ selected: [OPTIONS[5] as Tag] })
      await userEvent.click(screen.getByRole('button', { name: 'Remove tag urgent' }))

      expect(onDeselect).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({ name: 'urgent' }),
      )
    })

    it('removes the last chip on Backspace in an empty field', async () => {
      // The convention every chip input shares. Correcting a mis-click needs no mouse.
      const { onDeselect } = setup({ selected: [OPTIONS[1] as Tag, OPTIONS[5] as Tag] })
      await userEvent.click(combobox())
      await userEvent.keyboard('{Backspace}')

      expect(onDeselect).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({ name: 'urgent' }),
      )
    })

    it('does not remove a chip when Backspace is editing text', async () => {
      const { onDeselect } = setup({ selected: [OPTIONS[1] as Tag] })
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'x')
      await userEvent.keyboard('{Backspace}')

      expect(onDeselect).not.toHaveBeenCalled()
    })
  })

  describe('create-on-the-fly', () => {
    it('offers to create a name that matches nothing', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'flaky')

      expect(screen.getByRole('option', { name: /Create.*flaky/ })).toBeInTheDocument()
    })

    it('creates on Enter when the create row is highlighted', async () => {
      const { onCreate } = setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'flaky')
      // One option-less list: the create row is the only row, at index 0.
      await userEvent.keyboard('{Enter}')

      expect(onCreate).toHaveBeenCalledExactlyOnceWith('flaky')
    })

    it('does not offer to create a tag that already exists', async () => {
      // The server's names are COLLATE NOCASE, so this would be a 409. Offering it is
      // offering a button that cannot work.
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'urgent')

      expect(screen.queryByRole('option', { name: /Create/ })).not.toBeInTheDocument()
    })

    it('does not offer to create a tag that exists in another case', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'URGENT')

      expect(screen.queryByRole('option', { name: /Create/ })).not.toBeInTheDocument()
    })

    it('offers the hyphenated name for a name with a space, and creates that', async () => {
      // The rule, shown at the moment it applies rather than as a 422 after the round trip
      // — and never silently applied, so the user sees what they are agreeing to.
      const { onCreate } = setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'needs review')

      const create = screen.getByRole('option', { name: /Create/ })
      expect(create).toHaveTextContent('needs-review')
      expect(create).toHaveTextContent('cannot contain spaces')

      await userEvent.click(create)
      expect(onCreate).toHaveBeenCalledExactlyOnceWith('needs-review')
    })

    it('never sends a name with a space to the server', async () => {
      const { onCreate } = setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'a b c')
      await userEvent.keyboard('{Enter}')

      expect(onCreate).toHaveBeenCalledExactlyOnceWith('a-b-c')
      for (const call of onCreate.mock.calls) {
        expect(call[0]).not.toMatch(/\s/u)
      }
    })

    it('refuses when the hyphenated name is one that already exists', async () => {
      // `needs review` -> `needs-review`, which is already there. Creating it would 409, so
      // the picker says to pick it instead.
      setup({ options: [usage('needs-review')] })
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'needs review')

      expect(screen.queryByRole('option', { name: /Create/ })).not.toBeInTheDocument()
      expect(screen.getByRole('alert')).toHaveTextContent('already exists')
    })

    it('reports an over-long name rather than offering to create it', async () => {
      const { onCreate } = setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'a'.repeat(51))

      expect(screen.getByRole('alert')).toHaveTextContent('50 characters or fewer')
      expect(screen.queryByRole('option', { name: /Create/ })).not.toBeInTheDocument()
      expect(onCreate).not.toHaveBeenCalled()
    })

    it('marks the input invalid when the name is refused', async () => {
      setup()
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'a'.repeat(51))

      expect(combobox()).toHaveAttribute('aria-invalid', 'true')
      expect(combobox()).toHaveAccessibleDescription(/50 characters or fewer/)
    })

    it('offers nothing to create when canCreate is false', async () => {
      setup({ canCreate: false })
      await userEvent.click(combobox())
      await userEvent.type(combobox(), 'flaky')

      expect(screen.queryByRole('option', { name: /Create/ })).not.toBeInTheDocument()
    })
  })
})
