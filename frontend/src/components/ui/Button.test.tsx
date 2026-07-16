import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Button } from './Button'

describe('Button', () => {
  it('renders its label and calls onClick', async () => {
    const onClick = vi.fn()
    render(<Button onClick={onClick}>Create card</Button>)

    await userEvent.click(screen.getByRole('button', { name: 'Create card' }))

    expect(onClick).toHaveBeenCalledOnce()
  })

  it('defaults to type="button" so it never submits a form by accident', () => {
    render(<Button>Cancel</Button>)
    expect(screen.getByRole('button')).toHaveAttribute('type', 'button')
  })

  it('honours an explicit type', () => {
    render(<Button type="submit">Save</Button>)
    expect(screen.getByRole('button')).toHaveAttribute('type', 'submit')
  })

  it('does not fire onClick when disabled', async () => {
    const onClick = vi.fn()
    render(
      <Button onClick={onClick} disabled>
        Delete
      </Button>,
    )

    await userEvent.click(screen.getByRole('button'))

    expect(onClick).not.toHaveBeenCalled()
  })

  describe('loading', () => {
    it('marks itself busy, disables itself, and keeps the label mounted', () => {
      render(<Button isLoading>Save</Button>)

      const button = screen.getByRole('button')
      expect(button).toHaveAttribute('aria-busy', 'true')
      expect(button).toBeDisabled()
      // The label stays in the tree (hidden) so the button keeps its width and the
      // pointer does not land on a resized target mid-click.
      expect(button).toHaveTextContent('Save')
    })

    it('blocks clicks while loading', async () => {
      const onClick = vi.fn()
      render(
        <Button isLoading onClick={onClick}>
          Save
        </Button>,
      )

      await userEvent.click(screen.getByRole('button'))

      expect(onClick).not.toHaveBeenCalled()
    })

    it('does not double-announce the busy state', () => {
      // aria-busy on the button already conveys it; the spinner must stay silent.
      render(<Button isLoading>Save</Button>)
      expect(screen.queryByRole('status')).not.toBeInTheDocument()
    })
  })

  it('renders icons on either side', () => {
    render(
      <Button iconBefore={<span data-testid="before" />} iconAfter={<span data-testid="after" />}>
        Move
      </Button>,
    )

    expect(screen.getByTestId('before')).toBeInTheDocument()
    expect(screen.getByTestId('after')).toBeInTheDocument()
  })

  it('names an icon-only button via aria-label', () => {
    render(<Button isIconOnly aria-label="Attach file" iconBefore={<span />} />)
    expect(screen.getByRole('button', { name: 'Attach file' })).toBeInTheDocument()
  })

  it.each(['primary', 'default', 'subtle', 'link', 'danger', 'warning'] as const)(
    'renders the %s appearance',
    (appearance) => {
      render(<Button appearance={appearance}>Label</Button>)
      expect(screen.getByRole('button')).toBeInTheDocument()
    },
  )

  it('forwards a ref to the underlying button', () => {
    const ref = { current: null as HTMLButtonElement | null }
    render(<Button ref={ref}>Label</Button>)
    expect(ref.current).toBeInstanceOf(HTMLButtonElement)
  })
})
