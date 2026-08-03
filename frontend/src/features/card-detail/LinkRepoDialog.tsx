import { X } from 'lucide-react'
import { type FormEvent, useEffect, useId, useRef, useState } from 'react'

import { Banner, Button, Input, Select } from '@/components/ui'
import { ICON } from '@/lib/icon'

import styles from './LinkRepoDialog.module.css'
import { useCredentialRepos, useGithubCredentials, useLinkRepo } from './queries'

export interface LinkRepoDialogProps {
  projectKey: string
  onClose: () => void
}

/**
 * Link a GitHub repository to the current project.
 *
 * A hand-built overlay, matching `AddKeyDialog` — there is no shared Modal primitive. Lists
 * the stored GitHub credentials to act with (admin-only on the server, which is why the
 * affordance that opens this is admin-gated), and takes the `owner`/`repo` to link; the
 * server fetches the repo with the token to resolve its id and default branch, so a repo the
 * token cannot see is rejected with the reason surfaced in the banner.
 */
export function LinkRepoDialog({ projectKey, onClose }: LinkRepoDialogProps) {
  const credentials = useGithubCredentials()
  const link = useLinkRepo(projectKey)

  const [credentialId, setCredentialId] = useState('')
  const [owner, setOwner] = useState('')
  const [repo, setRepo] = useState('')
  const [branchPrefix, setBranchPrefix] = useState('')

  const titleId = useId()
  const firstFieldRef = useRef<HTMLSelectElement>(null)

  useEffect(() => {
    firstFieldRef.current?.focus()
  }, [])

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  // The effective selection: the first credential until the user picks another. Derived, not
  // synced into state via an effect — no cascading render, and it is always fresh.
  const selectedCredentialId =
    credentialId !== '' ? credentialId : (credentials.data?.[0]?.id ?? '')

  const noCredentials = credentials.isSuccess && (credentials.data?.length ?? 0) === 0

  // The repo picker: an assistive shortcut, not the only way in. It fills owner/repo when a
  // repo is chosen, but both fields stay editable — a repo further back than the first page,
  // or reachable only by typing (the picker's own fetch failed), still works.
  const repos = useCredentialRepos(selectedCredentialId, selectedCredentialId !== '')
  const pushableRepos = (repos.data ?? []).filter((repo) => repo.canPush)
  const showPicker =
    selectedCredentialId !== '' && (repos.isPending || pushableRepos.length > 0)

  const canSubmit =
    selectedCredentialId !== '' &&
    owner.trim().length > 0 &&
    repo.trim().length > 0 &&
    !link.isPending

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (!canSubmit) return
    const trimmedPrefix = branchPrefix.trim()
    link.mutate(
      {
        credentialId: selectedCredentialId,
        owner: owner.trim(),
        repo: repo.trim(),
        branchPrefix: trimmedPrefix === '' ? null : trimmedPrefix,
      },
      { onSuccess: () => onClose() },
    )
  }

  const options = (credentials.data ?? []).map((credential) => ({
    label: credential.label,
    value: credential.id,
  }))

  const repoOptions = pushableRepos.map((repo) => ({
    label: repo.private ? `${repo.fullName} (private)` : repo.fullName,
    value: String(repo.id),
  }))

  function onPickRepo(id: string) {
    const picked = pushableRepos.find((repo) => String(repo.id) === id)
    if (!picked) return
    const [pickedOwner, pickedRepo] = picked.fullName.split('/')
    setOwner(pickedOwner ?? '')
    setRepo(pickedRepo ?? '')
  }

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
            Link a GitHub repository
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
          {link.isError && (
            <Banner appearance="error">
              {link.error.problem?.detail ?? 'Could not link the repository.'}
            </Banner>
          )}

          {noCredentials ? (
            <p className={styles.note}>
              No GitHub credential is stored yet. Add a personal access token under{' '}
              <strong>Integrations</strong>, then link a repository.
            </p>
          ) : (
            <Select
              ref={firstFieldRef}
              label="Credential"
              isRequired
              value={selectedCredentialId}
              onChange={(event) => setCredentialId(event.target.value)}
              options={options}
              placeholder={credentials.isPending ? 'Loading…' : 'Choose a credential'}
              helpMessage="The GitHub token Atlas acts with."
            />
          )}

          {showPicker && (
            <Select
              label="Pick a repository"
              value=""
              onChange={(event) => onPickRepo(event.target.value)}
              options={repoOptions}
              placeholder={repos.isPending ? 'Loading…' : 'Choose one, or type below'}
              helpMessage="Fills in owner/repository below. Only your 30 most recently pushed repos are listed — type below for anything further back."
            />
          )}

          <Input
            label="Owner"
            isRequired
            value={owner}
            onChange={(event) => setOwner(event.target.value)}
            placeholder="e.g. octocat"
            autoComplete="off"
            spellCheck={false}
            disabled={noCredentials}
          />
          <Input
            label="Repository"
            isRequired
            value={repo}
            onChange={(event) => setRepo(event.target.value)}
            placeholder="e.g. hello-world"
            autoComplete="off"
            spellCheck={false}
            disabled={noCredentials}
          />
          <Input
            label="Branch prefix"
            value={branchPrefix}
            onChange={(event) => setBranchPrefix(event.target.value)}
            placeholder="feature"
            helpMessage="Prefix for generated branch names. Defaults to “feature”."
            autoComplete="off"
            spellCheck={false}
            disabled={noCredentials}
          />

          <div className={styles.actions}>
            <Button appearance="subtle" onClick={onClose} type="button">
              Cancel
            </Button>
            <Button
              appearance="primary"
              type="submit"
              isLoading={link.isPending}
              disabled={!canSubmit}
            >
              Link repository
            </Button>
          </div>
        </form>
      </div>
    </div>
  )
}
