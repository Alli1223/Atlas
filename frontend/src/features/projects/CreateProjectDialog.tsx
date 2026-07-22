import { useNavigate } from '@tanstack/react-router'
import { X } from 'lucide-react'
import { type FormEvent, useEffect, useId, useRef, useState } from 'react'

import { Banner, Button, Input, Select } from '@/components/ui'
import { ICON } from '@/lib/icon'

import type { Template } from './api'
import styles from './CreateProjectDialog.module.css'
import { useCreateProject, useTemplates } from './queries'

/** Human labels for the template ids the backend ships. */
const TEMPLATE_LABEL: Record<string, string> = {
  programming: 'Software',
  '3d-modeling': '3D modeling',
  'job-search': 'Job search',
  blank: 'Blank',
}

/** Derives a plausible key from a name: first letters / uppercased alphanumerics. */
function suggestKey(name: string): string {
  const cleaned = name.toUpperCase().replace(/[^A-Z0-9 ]/g, '')
  const words = cleaned.split(/\s+/).filter(Boolean)
  if (words.length === 0) return ''
  if (words.length === 1) return words[0]!.slice(0, 6)
  return words
    .map((w) => w[0])
    .join('')
    .slice(0, 6)
}

export interface CreateProjectDialogProps {
  onClose: () => void
}

/**
 * A create-project dialog. Deliberately small — the key is fixed at creation and can never
 * change (it prefixes every card key forever), so this is the one screen that sets it.
 *
 * A hand-built overlay rather than a shared Modal primitive: the modal primitive is the
 * card-detail agent's, and the board phase needs exactly one dialog. Focus is trapped to
 * the dialog only in the loose sense that Escape and the blanket close it; the first field
 * is focused on open.
 */
export function CreateProjectDialog({ onClose }: CreateProjectDialogProps) {
  const navigate = useNavigate()
  const templates = useTemplates()
  const createProject = useCreateProject()

  const [name, setName] = useState('')
  const [key, setKey] = useState('')
  const [keyEdited, setKeyEdited] = useState(false)
  const [template, setTemplate] = useState<Template>('programming')

  const titleId = useId()
  const nameRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    nameRef.current?.focus()
  }, [])

  // Escape closes, matching the blanket click.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  const effectiveKey = keyEdited ? key : suggestKey(name)

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    createProject.mutate(
      {
        name: name.trim(),
        key: effectiveKey.trim(),
        template,
      },
      {
        onSuccess: (project) => {
          onClose()
          void navigate({
            to: '/projects/$projectKey/board',
            params: { projectKey: project.key },
          })
        },
      },
    )
  }

  const templateOptions = (templates.data ?? []).map((t) => ({
    label: TEMPLATE_LABEL[t.id] ?? t.id,
    value: t.id,
  }))

  return (
    <div className={styles.blanket} onClick={onClose}>
      <div
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(event) => event.stopPropagation()}
      >
        <header className={styles.header}>
          <h2 id={titleId} className={styles.title}>
            Create project
          </h2>
          <Button
            appearance="subtle"
            isIconOnly
            aria-label="Close"
            onClick={onClose}
            iconBefore={<X {...ICON} aria-hidden="true" />}
          />
        </header>

        <form className={styles.form} onSubmit={onSubmit}>
          {createProject.isError && (
            <Banner appearance="error">
              {createProject.error.problem?.detail ?? 'Could not create the project.'}
            </Banner>
          )}

          <Input
            ref={nameRef}
            label="Name"
            isRequired
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Atlas Platform"
          />

          <Input
            label="Key"
            isRequired
            value={effectiveKey}
            onChange={(event) => {
              setKeyEdited(true)
              setKey(event.target.value.toUpperCase())
            }}
            helpMessage="Prefixes every card key, e.g. ATLAS-1. Cannot be changed later."
            placeholder="ATLAS"
          />

          {templateOptions.length > 0 && (
            <Select
              label="Template"
              value={template}
              onChange={(event) => setTemplate(event.target.value as Template)}
              options={templateOptions}
              helpMessage="Seeds the project's card types, statuses, priorities and tags."
            />
          )}

          <div className={styles.actions}>
            <Button appearance="subtle" onClick={onClose} type="button">
              Cancel
            </Button>
            <Button
              appearance="primary"
              type="submit"
              isLoading={createProject.isPending}
              disabled={name.trim().length === 0 || effectiveKey.trim().length === 0}
            >
              Create project
            </Button>
          </div>
        </form>
      </div>
    </div>
  )
}
