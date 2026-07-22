import type { Editor, Range } from '@tiptap/core'
import type { SuggestionOptions } from '@tiptap/suggestion'

import styles from './MarkdownEditor.module.css'

/** A person the @-menu can offer. */
export interface MentionCandidate {
  id: string
  label: string
}

/**
 * The @-mention dropdown for the editor, built without a positioning library.
 *
 * TipTap's `Mention` needs a `suggestion` config: `items(query)` to filter, and `render()`
 * returning lifecycle handlers. Blog examples reach for `tippy.js`, but it is one more
 * dependency for a list that only ever needs to sit under the caret — so this positions a
 * plain absolutely-placed element from the caret's client rect, and cleans it up on exit.
 * Keyboard-first (arrows + enter), because that is how you @-someone without leaving the
 * keyboard mid-sentence.
 */
export function mentionSuggestion(
  getCandidates: () => MentionCandidate[],
): Omit<SuggestionOptions<MentionCandidate>, 'editor'> {
  return {
    char: '@',
    items: ({ query }) => {
      const q = query.toLowerCase()
      return getCandidates()
        .filter((c) => c.label.toLowerCase().includes(q))
        .slice(0, 8)
    },
    command: ({ editor, range, props }: { editor: Editor; range: Range; props: MentionCandidate }) => {
      // Insert the mention node, then a trailing space so typing continues cleanly.
      editor
        .chain()
        .focus()
        .insertContentAt(range, [
          { type: 'mention', attrs: { id: props.id, label: props.label } },
          { type: 'text', text: ' ' },
        ])
        .run()
    },
    render: () => {
      let element: HTMLDivElement | null = null
      let items: MentionCandidate[] = []
      let active = 0
      let onPick: (item: MentionCandidate) => void = () => undefined

      const paint = () => {
        if (!element) return
        element.replaceChildren()
        if (items.length === 0) {
          element.hidden = true
          return
        }
        element.hidden = false
        items.forEach((item, index) => {
          const option = document.createElement('button')
          option.type = 'button'
          option.textContent = item.label
          option.className =
            index === active
              ? `${styles.mentionOption ?? ''} ${styles.mentionActive ?? ''}`
              : (styles.mentionOption ?? '')
          // mousedown, not click: the editor keeps focus so the range is still valid.
          option.addEventListener('mousedown', (event) => {
            event.preventDefault()
            onPick(item)
          })
          element!.append(option)
        })
      }

      const place = (rect: DOMRect | null | undefined) => {
        if (!element || !rect) return
        element.style.left = `${rect.left}px`
        element.style.top = `${rect.bottom + 4}px`
      }

      return {
        onStart: (props) => {
          items = props.items
          active = 0
          onPick = (item) => props.command(item)
          element = document.createElement('div')
          element.className = styles.mentionMenu ?? ''
          document.body.append(element)
          paint()
          place(props.clientRect?.())
        },
        onUpdate: (props) => {
          items = props.items
          active = 0
          onPick = (item) => props.command(item)
          paint()
          place(props.clientRect?.())
        },
        onKeyDown: (props) => {
          if (items.length === 0) return false
          if (props.event.key === 'ArrowDown') {
            active = (active + 1) % items.length
            paint()
            return true
          }
          if (props.event.key === 'ArrowUp') {
            active = (active - 1 + items.length) % items.length
            paint()
            return true
          }
          if (props.event.key === 'Enter') {
            const chosen = items[active]
            if (chosen) onPick(chosen)
            return true
          }
          if (props.event.key === 'Escape') {
            element?.remove()
            element = null
            return true
          }
          return false
        },
        onExit: () => {
          element?.remove()
          element = null
        },
      }
    },
  }
}
