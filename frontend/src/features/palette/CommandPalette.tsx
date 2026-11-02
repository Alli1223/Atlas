import { useNavigate } from '@tanstack/react-router'
import { useEffect, useMemo, useRef, useState } from 'react'

import { useProjects } from '@/features/projects'
import kit from '@/styles/kit.module.css'

import { type Command, commandsFor } from './commands'

/** Whether a key event is aimed at something the user is typing in. */
export function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  return (
    target.isContentEditable ||
    ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) ||
    target.closest('[contenteditable="true"]') !== null
  )
}

const SHORTCUTS: [string, string][] = [
  ['.  or  Ctrl/⌘ K', 'Command palette'],
  ['/', 'Search'],
  ['c', 'Create (projects)'],
  ['g h', 'Go to overview'],
  ['g p', 'Go to projects'],
  ['g s', 'Go to search'],
  ['?', 'This help'],
  ['Esc', 'Close'],
]

/**
 * The command palette (`.` or Ctrl/⌘-K) and the global shortcuts (`TODO.md` Phase 19) — one
 * listener, so the two can never disagree about what a key does.
 */
export function CommandPalette() {
  const [open, setOpen] = useState(false)
  const [help, setHelp] = useState(false)
  const [query, setQuery] = useState('')
  const [index, setIndex] = useState(0)
  const navigate = useNavigate()
  const projects = useProjects()
  const input = useRef<HTMLInputElement>(null)
  const pendingG = useRef(false)

  const commands = useMemo(
    () => commandsFor(query, (projects.data ?? []).map((p) => ({ key: p.key, name: p.name }))),
    [query, projects.data],
  )

  const go = (command: Command) => {
    setOpen(false)
    setQuery('')
    void navigate({ to: command.to, params: command.params, search: command.search } as never)
  }

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey
      if (mod && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        setOpen((value) => !value)
        return
      }
      if (event.key === 'Escape') {
        setOpen(false)
        setHelp(false)
        return
      }
      if (mod || event.altKey || isTyping(event.target)) return
      if (pendingG.current) {
        pendingG.current = false
        const to = { h: '/', p: '/projects', s: '/search' }[event.key]
        if (to) {
          event.preventDefault()
          void navigate({ to } as never)
        }
        return
      }
      switch (event.key) {
        case '.':
          event.preventDefault()
          setIndex(0)
          setOpen(true)
          break
        case '/':
          event.preventDefault()
          void navigate({ to: '/search' })
          break
        case '?':
          setHelp((value) => !value)
          break
        case 'c':
          void navigate({ to: '/projects' })
          break
        case 'g':
          pendingG.current = true
          setTimeout(() => {
            pendingG.current = false
          }, 1000)
          break
        default:
          break
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [navigate])

  useEffect(() => {
    if (open) input.current?.focus()
  }, [open])

  if (help) {
    return (
      <div className={kit.overlay} onClick={() => setHelp(false)} role="presentation">
        <div className={kit.dialog} role="dialog" aria-label="Keyboard shortcuts" onClick={(e) => e.stopPropagation()}>
          <h2 className={kit.subtitle}>Keyboard shortcuts</h2>
          <table className={kit.table}>
            <tbody>
              {SHORTCUTS.map(([keys, action]) => (
                <tr key={keys}>
                  <td>
                    <kbd className={kit.kbd}>{keys}</kbd>
                  </td>
                  <td>{action}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    )
  }

  if (!open) return null
  return (
    <div className={kit.overlay} onClick={() => setOpen(false)} role="presentation">
      <div className={kit.dialog} role="dialog" aria-label="Command palette" onClick={(e) => e.stopPropagation()}>
        <input
          ref={input}
          className={kit.textarea}
          style={{ minHeight: 0 }}
          placeholder="Type a command, a project, or a card key…"
          aria-label="Command"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value)
            setIndex(0)
          }}
          onKeyDown={(event) => {
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              setIndex((i) => Math.min(i + 1, commands.length - 1))
            } else if (event.key === 'ArrowUp') {
              event.preventDefault()
              setIndex((i) => Math.max(i - 1, 0))
            } else if (event.key === 'Enter') {
              const command = commands[index]
              if (command) go(command)
            }
          }}
        />
        <div role="listbox" aria-label="Commands">
          {commands.slice(0, 50).map((command, i) => (
            <button
              key={command.id}
              type="button"
              role="option"
              aria-selected={i === index}
              className={kit.option}
              onMouseEnter={() => setIndex(i)}
              onClick={() => go(command)}
            >
              <span className={kit.grow}>{command.label}</span>
              {command.hint && <kbd className={kit.kbd}>{command.hint}</kbd>}
            </button>
          ))}
        </div>
      </div>
    </div>
  )
}
