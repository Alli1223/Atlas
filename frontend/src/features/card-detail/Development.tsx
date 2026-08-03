import { GitBranch, GitCommitHorizontal, GitPullRequest, type LucideIcon, Plus } from 'lucide-react'
import { useState } from 'react'

import { Banner, Button, Lozenge, type LozengeAppearance, Spinner } from '@/components/ui'
import { useCurrentUser } from '@/features/auth/queries'
import { ICON, ICON_SMALL } from '@/lib/icon'

import type { Card, CiState, ReviewState } from './api'
import styles from './Development.module.css'
import { LinkRepoDialog } from './LinkRepoDialog'
import {
  useCardActivity,
  useCardGitLinks,
  useCreateBranch,
  useCreatePr,
  useProjectRepo,
  useUnlinkRepo,
} from './queries'

/** The lucide glyph for a git link, by its kind. `GitBranch` is the sensible default. */
function kindIcon(kind: string): LucideIcon {
  if (kind === 'pr') return GitPullRequest
  if (kind === 'commit') return GitCommitHorizontal
  return GitBranch
}

/** The CI badge's colour, borrowed from the same status-pill idiom as a card's status field. */
const CI_APPEARANCE: Record<CiState, LozengeAppearance> = {
  passed: 'success',
  running: 'inprogress',
  failed: 'removed',
  neutral: 'default',
}

const CI_LABEL: Record<CiState, string> = {
  passed: 'Checks passed',
  running: 'Checks running',
  failed: 'Checks failed',
  neutral: 'No checks',
}

/** The review badge's colour. `changesrequested` (no camelCase) is serde's `rename_all =
 * "lowercase"` output for `ChangesRequested` — not a typo. */
const REVIEW_APPEARANCE: Record<ReviewState, LozengeAppearance> = {
  approved: 'success',
  changesrequested: 'removed',
  pending: 'default',
}

const REVIEW_LABEL: Record<ReviewState, string> = {
  approved: 'Approved',
  changesrequested: 'Changes requested',
  pending: 'Review pending',
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
  const createPr = useCreatePr(card.key)
  const unlink = useUnlinkRepo(projectKey)
  const [linking, setLinking] = useState(false)

  // A PR needs a branch to open from, and there is nothing left to do once one is already
  // recorded — the action would just be idempotent, so it disappears rather than inviting a
  // pointless click.
  const hasBranch = links.data?.some((link) => link.kind === 'branch') ?? false
  const hasPr = links.data?.some((link) => link.kind === 'pr') ?? false

  // Live GitHub data — nothing here is stored, so it is only worth asking for once a branch
  // exists, and it self-refreshes while a check is still running.
  const activity = useCardActivity(card.key, hasBranch)

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
            {hasBranch && !hasPr && (
              <Button
                appearance="default"
                size="compact"
                iconBefore={<GitPullRequest {...ICON} aria-hidden="true" />}
                isLoading={createPr.isPending}
                onClick={() => createPr.mutate()}
              >
                Create PR
              </Button>
            )}
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
          {createPr.isError && (
            <Banner appearance="error">
              {createPr.error.problem?.detail ?? 'Could not open the pull request.'}
            </Banner>
          )}

          {(activity.data?.ciStatus ?? activity.data?.reviewState) && (
            <div className={styles.badges}>
              {activity.data.ciStatus && (
                <Lozenge appearance={CI_APPEARANCE[activity.data.ciStatus]} isBold>
                  {CI_LABEL[activity.data.ciStatus]}
                </Lozenge>
              )}
              {activity.data.reviewState && (
                <Lozenge appearance={REVIEW_APPEARANCE[activity.data.reviewState]} isBold>
                  {REVIEW_LABEL[activity.data.reviewState]}
                </Lozenge>
              )}
            </div>
          )}

          {/* Strictly `=== false`: `null`/`undefined` means "not yet known", never
              "conflicts" (docs/research/github-api.md §10) — showing a warning there would
              be a phantom conflict on every freshly-opened PR. */}
          {activity.data?.mergeable === false && (
            <Banner appearance="warning">This PR has merge conflicts.</Banner>
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

          {activity.data && activity.data.commits.length > 0 && (
            <ul className={styles.links}>
              {activity.data.commits.map((commit) => (
                <li key={commit.sha} className={styles.link}>
                  <GitCommitHorizontal {...ICON_SMALL} aria-hidden="true" />
                  <a
                    className={styles.linkRef}
                    href={commit.htmlUrl}
                    target="_blank"
                    rel="noreferrer"
                    title={commit.message}
                  >
                    {commit.message}
                  </a>
                </li>
              ))}
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
