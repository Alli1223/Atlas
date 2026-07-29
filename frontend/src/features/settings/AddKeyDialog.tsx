import { X } from 'lucide-react'
import { type FormEvent, useEffect, useId, useRef, useState } from 'react'

import { Banner, Button, Input } from '@/components/ui'
import { ICON } from '@/lib/icon'

import type { Credential, Provider } from './api'
import styles from './AddKeyDialog.module.css'
import { useCreateCredential, useReplaceCredential } from './queries'
import { PROVIDER_META } from './status'

export interface AddKeyDialogProps {
  provider: Provider
  /** When set, the dialog replaces this credential instead of adding a new one. */
  replacing?: Credential
  onClose: () => void
}

/**
 * Add-or-replace a provider key.
 *
 * # The one rule that dominates this component
 *
 * **The secret is write-only.** It lives in a `password`-type input, it is never rendered
 * anywhere else, and nothing reads it back out — not on success, not into a "here's what
 * you entered" confirmation, nowhere. The create response is metadata only (it has no field
 * for the secret), so there is nothing to echo even by accident, and on success the whole
 * dialog unmounts, taking the input's value with it. A test asserts the entered key never
 * reappears in the DOM after submit.
 *
 * A hand-built overlay rather than a shared Modal primitive, matching `CreateProjectDialog`
 * — the Modal primitive belongs to the card-detail feature and this screen needs exactly
 * one dialog.
 */
export function AddKeyDialog({ provider, replacing, onClose }: AddKeyDialogProps) {
  const meta = PROVIDER_META[provider]
  const isReplacing = replacing !== undefined

  const createCredential = useCreateCredential()
  const replaceCredential = useReplaceCredential()
  const mutation = isReplacing ? replaceCredential : createCredential

  const [label, setLabel] = useState(replacing?.label ?? '')
  const [secret, setSecret] = useState('')

  const titleId = useId()
  const firstFieldRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    firstFieldRef.current?.focus()
  }, [])

  // Escape closes, matching the blanket click.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  const trimmedLabel = label.trim()
  const canSubmit = trimmedLabel.length > 0 && secret.trim().length > 0 && !mutation.isPending

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (!canSubmit) return

    const next = { provider, label: trimmedLabel, secret }

    if (isReplacing) {
      replaceCredential.mutate({ oldId: replacing.id, next }, { onSuccess: () => onClose() })
    } else {
      createCredential.mutate(next, { onSuccess: () => onClose() })
    }
  }

  const heading = isReplacing ? `Replace ${meta.name} key` : `Add a ${meta.name} key`

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
            {heading}
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
          {mutation.isError && (
            <Banner appearance="error">
              {mutation.error.problem?.detail ?? 'Could not save the key.'}
            </Banner>
          )}

          {isReplacing && (
            <p className={styles.replaceNote}>
              The existing key ending <code>…{replacing.lastFour}</code> will be removed and
              this one stored in its place.
            </p>
          )}

          <Input
            ref={firstFieldRef}
            label="Label"
            isRequired
            value={label}
            onChange={(event) => setLabel(event.target.value)}
            helpMessage={`A name to tell this key apart, unique within ${meta.name}.`}
            placeholder="e.g. work laptop"
            autoComplete="off"
          />

          <Input
            label={meta.secretNoun[0]!.toUpperCase() + meta.secretNoun.slice(1)}
            type="password"
            isRequired
            value={secret}
            onChange={(event) => setSecret(event.target.value)}
            placeholder={meta.placeholder}
            helpMessage="Stored encrypted. Shown once, here — it is never displayed again."
            autoComplete="off"
            spellCheck={false}
          />

          <div className={styles.actions}>
            <Button appearance="subtle" onClick={onClose} type="button">
              Cancel
            </Button>
            <Button
              appearance="primary"
              type="submit"
              isLoading={mutation.isPending}
              disabled={!canSubmit}
            >
              {isReplacing ? 'Replace key' : 'Add key'}
            </Button>
          </div>
        </form>
      </div>
    </div>
  )
}
