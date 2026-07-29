import { CheckCircle2, KeyRound, Plus, RefreshCw, RotateCcw, Trash2 } from 'lucide-react'
import { useState } from 'react'

import { Button } from '@/components/ui'
import { ICON } from '@/lib/icon'

import { AddKeyDialog } from './AddKeyDialog'
import type { Credential, Provider } from './api'
import { expiryPhrase, relativeTime } from './format'
import styles from './ProviderSection.module.css'
import { useDeleteCredential, useValidateCredential } from './queries'
import { PROVIDER_META } from './status'
import { StatusPill } from './StatusPill'

/** The metadata line under a key: last-4, expiry, and when it was last checked. */
function KeyMeta({ credential }: { credential: Credential }) {
  const expiry = expiryPhrase(credential.expiresAt)

  return (
    <div className={styles.meta}>
      <span className={styles.lastFour}>
        ends <code>…{credential.lastFour}</code>
      </span>
      {expiry !== null && (
        <>
          <span className={styles.dot} aria-hidden="true" />
          <span
            className={credential.status === 'expired' ? styles.metaDanger : undefined}
            {...(credential.expiresAt != null
              ? { title: new Date(credential.expiresAt).toLocaleString() }
              : {})}
          >
            {expiry}
          </span>
        </>
      )}
      <span className={styles.dot} aria-hidden="true" />
      <span
        {...(credential.lastValidatedAt != null
          ? { title: new Date(credential.lastValidatedAt).toLocaleString() }
          : {})}
      >
        {credential.lastValidatedAt != null
          ? `checked ${relativeTime(credential.lastValidatedAt)}`
          : 'never checked'}
      </span>
    </div>
  )
}

/** The discovered provider scopes, as small chips. Nothing renders when none are known. */
function Scopes({ scopes }: { scopes: readonly string[] }) {
  if (scopes.length === 0) return null

  return (
    <ul className={styles.scopes} aria-label="Scopes">
      {scopes.map((scope) => (
        <li key={scope} className={styles.scope}>
          {scope}
        </li>
      ))}
    </ul>
  )
}

function KeyRow({ credential }: { credential: Credential }) {
  const validate = useValidateCredential()
  const remove = useDeleteCredential()
  const [replacing, setReplacing] = useState(false)
  const [confirmingDelete, setConfirmingDelete] = useState(false)

  return (
    <li className={styles.keyRow}>
      <div className={styles.keyHead}>
        <StatusPill status={credential.status} />
        <span className={styles.label}>{credential.label}</span>
      </div>

      <KeyMeta credential={credential} />
      <Scopes scopes={credential.scopes} />

      <div className={styles.keyActions}>
        <Button
          appearance="subtle"
          size="compact"
          isLoading={validate.isPending}
          onClick={() => validate.mutate(credential.id)}
          iconBefore={<RefreshCw {...ICON} aria-hidden="true" />}
        >
          Validate now
        </Button>
        <Button
          appearance="subtle"
          size="compact"
          onClick={() => setReplacing(true)}
          iconBefore={<RotateCcw {...ICON} aria-hidden="true" />}
        >
          Replace
        </Button>

        {confirmingDelete ? (
          <span className={styles.confirm}>
            <span className={styles.confirmText}>Delete this key?</span>
            <Button
              appearance="danger"
              size="compact"
              isLoading={remove.isPending}
              onClick={() => remove.mutate(credential.id)}
            >
              Delete
            </Button>
            <Button appearance="subtle" size="compact" onClick={() => setConfirmingDelete(false)}>
              Cancel
            </Button>
          </span>
        ) : (
          <Button
            appearance="subtle"
            size="compact"
            onClick={() => setConfirmingDelete(true)}
            iconBefore={<Trash2 {...ICON} aria-hidden="true" />}
          >
            Delete
          </Button>
        )}
      </div>

      {validate.isError && (
        <p className={styles.rowError} role="alert">
          {validate.error.problem?.detail ?? 'Could not validate the key.'}
        </p>
      )}
      {remove.isError && (
        <p className={styles.rowError} role="alert">
          {remove.error.problem?.detail ?? 'Could not delete the key.'}
        </p>
      )}

      {replacing && (
        <AddKeyDialog
          provider={credential.provider}
          replacing={credential}
          onClose={() => setReplacing(false)}
        />
      )}
    </li>
  )
}

export interface ProviderSectionProps {
  provider: Provider
  credentials: readonly Credential[]
}

/**
 * One provider's card: its name and blurb, every key stored for it, and the controls to
 * add another. A provider with no key still renders — as a calm "not configured" prompt —
 * because the four providers are a fixed set the user should always see, whether or not
 * they have wired one up yet.
 */
export function ProviderSection({ provider, credentials }: ProviderSectionProps) {
  const meta = PROVIDER_META[provider]
  const [adding, setAdding] = useState(false)
  const hasKeys = credentials.length > 0

  return (
    <section className={styles.section} aria-labelledby={`provider-${provider}`}>
      <header className={styles.sectionHead}>
        <span className={styles.icon} aria-hidden="true">
          {hasKeys ? <CheckCircle2 {...ICON} /> : <KeyRound {...ICON} />}
        </span>
        <div className={styles.sectionTitle}>
          <h2 id={`provider-${provider}`} className={styles.name}>
            {meta.name}
          </h2>
          <p className={styles.blurb}>{meta.blurb}</p>
        </div>
        <Button
          appearance={hasKeys ? 'subtle' : 'default'}
          size="compact"
          onClick={() => setAdding(true)}
          iconBefore={<Plus {...ICON} aria-hidden="true" />}
        >
          {hasKeys ? 'Add another' : 'Add key'}
        </Button>
      </header>

      {hasKeys ? (
        <ul className={styles.keys}>
          {credentials.map((credential) => (
            <KeyRow key={credential.id} credential={credential} />
          ))}
        </ul>
      ) : (
        <p className={styles.empty}>Not configured — add a {meta.secretNoun} to enable this.</p>
      )}

      {adding && <AddKeyDialog provider={provider} onClose={() => setAdding(false)} />}
    </section>
  )
}
