import { GitBranch, GitCommitHorizontal, GitPullRequest, type LucideIcon, Plus } from 'lucide-react'
import { useState } from 'react'

import { Banner, Button, Spinner } from '@/components/ui'
import { useCurrentUser } from '@/features/auth/queries'
import { ICON, ICON_SMALL } from '@/lib/icon'

import type { Card } from './api'
import styles from './Development.module.css'
import { LinkRepoDialog } from './LinkRepoDialog'
import { useCardGitLinks, useCreateBranch, useProjectRepo, useUnlinkRepo } from './queries'

/** The lucide glyph for a git link, by its kind. `GitBranch` is the sensible default. */
function kindIcon(kind: string): LucideIcon {
  if (kind === 'pr') return GitPullRequest
  if (kind === 'commit') return GitCommitHorizontal
  return GitBranch
}

/**
 * The card's "Development" panel: the linked repo, a Create-branch action, and the branches
 * / PRs / commits tied to the card.
 *
 * Reads the project↔repo link (a 404 means "not linked", folded to `null` in the query) and
 * the card's git links. Creating a branch is a single click — the card's key and summary are
 * the branch name, so there is nothing to fill in. Linking a repo is an admin action (it
 * lists credentials, which is admin-only), so the "Link a repo" affordance is gated on it.
 */
export function Development({ card, projectKey }: { card: Card; projectKey: string }) {
  const { user } = useCurrentUser()
  const isAdmin = user?.role === 'admin'

  const repo = useProjectRepo(projectKey)
  const links = useCardGitLinks(card.key)
  const createBranch = useCreateBranch(card.key)
  const unlink = useUnlinkRepo(projectKey)
  const [linking, setLinking] = useState(false)

  return (
    <section className={styles.development} aria-labelledby={`dev-${card.key}`}>
      <span id={`dev-${card.key}`} className={styles.label}>
        Development
      </span>

      {repo.isPending ? (
        <Spinner label="Loading repository" />
      ) : repo.data ? (
        <div className={styles.body}>
          <a
            className={styles.repo}
            href={`https://github.com/${repo.data.fullName}`}
            target="_blank"
            rel="noreferrer"
            title={repo.data.fullName}
          >
            <GitBranch {...ICON_SMALL} aria-hidden="true" />
            <span className={styles.repoName}>{repo.data.fullName}</span>
          </a>

          <div className={styles.actions}>
            <Button
              appearance="default"
              size="compact"
              iconBefore={<GitBranch {...ICON} aria-hidden="true" />}
              isLoading={createBranch.isPending}
              onClick={() => createBranch.mutate()}
            >
              Create branch
            </Button>
            {isAdmin && (
              <Button
                appearance="subtle"
                size="compact"
                isLoading={unlink.isPending}
                onClick={() => unlink.mutate()}
              >
                Unlink
              </Button>
            )}
          </div>

          {createBranch.isError && (
            <Banner appearance="error">
              {createBranch.error.problem?.detail ?? 'Could not create the branch.'}
            </Banner>
          )}

          {links.data && links.data.length > 0 && (
            <ul className={styles.links}>
              {links.data.map((link) => {
                const Icon = kindIcon(link.kind)
                return (
                  <li key={`${link.kind}:${link.reference}`} className={styles.link}>
                    <Icon {...ICON_SMALL} aria-hidden="true" />
                    {link.url ? (
                      <a
                        className={styles.linkRef}
                        href={link.url}
                        target="_blank"
                        rel="noreferrer"
                        title={link.reference}
                      >
                        {link.reference}
                      </a>
                    ) : (
                      <span className={styles.linkRef} title={link.reference}>
                        {link.reference}
                      </span>
                    )}
                  </li>
                )
              })}
            </ul>
          )}
        </div>
      ) : (
        <div className={styles.body}>
          <p className={styles.empty}>No repository linked.</p>
          {isAdmin && (
            <Button
              appearance="subtle"
              size="compact"
              iconBefore={<Plus {...ICON} aria-hidden="true" />}
              onClick={() => setLinking(true)}
            >
              Link a repo
            </Button>
          )}
        </div>
      )}

      {linking && <LinkRepoDialog projectKey={projectKey} onClose={() => setLinking(false)} />}
    </section>
  )
}
