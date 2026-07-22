import { Link } from '@tanstack/react-router'
import { FileText } from 'lucide-react'
import { useState } from 'react'

import { Banner, Button, EmptyState, Spinner } from '@/components/ui'
import { useCurrentUser } from '@/features/auth/queries'
import type { ApiError } from '@/lib/api'
import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'

import styles from './CardDetail.module.css'
import { Comments } from './Comments'
import { HistoryTab } from './HistoryTab'
import { InlineText } from './InlineText'
import { MarkdownEditor } from './MarkdownEditor'
import { MarkdownView } from './MarkdownView'
import { isBlankMarkdown } from './markdown'
import type { MentionCandidate } from './mention-suggestion'
import {
  projectKeyOf,
  useCard,
  useCardIndex,
  useMembers,
  usePatchCard,
} from './queries'
import { Sidebar } from './Sidebar'

export interface CardDetailProps {
  cardKey: string
  /** Rendered in a modal (compact chrome) vs. a full page. @default false */
  isModal?: boolean
}

/**
 * The card detail view — Jira's two-column issue layout.
 *
 * Main column: an inline-editable summary, a rich-text description, and the comment thread.
 * Sidebar: status with its legal transitions, people, priority, tags, dates, parent. The
 * exact same component backs both the deep-linkable full-page route (`/cards/$key`) and the
 * modal, differing only in chrome — so there is one card view, not two that drift.
 */
export function CardDetail({ cardKey, isModal = false }: CardDetailProps) {
  const card = useCard(cardKey)
  const projectKey = projectKeyOf(cardKey)
  const { user } = useCurrentUser()
  const members = useMembers(projectKey)
  // Called unconditionally (rules of hooks); the query only fires once a parent is known.
  const cardIndex = useCardIndex(projectKey, card.data?.parentId != null)

  if (card.isPending) {
    return (
      <div className={styles.loadingRow}>
        <Spinner label="Loading card" />
      </div>
    )
  }

  if (card.isError || !card.data) {
    const error = card.error as ApiError | null
    const notFound = error?.status === 404
    return (
      <div className={styles.errorWrap}>
        <EmptyState
          header={notFound ? 'Card not found' : 'Could not load this card'}
          description={
            notFound
              ? `No card with key ${cardKey} — it may have been deleted or never existed.`
              : (error?.problem?.detail ?? 'Something went wrong reaching the server.')
          }
          primaryAction={
            <Link to="/">
              <Button appearance="primary">Back to overview</Button>
            </Link>
          }
        />
      </div>
    )
  }

  const data = card.data
  const candidates: MentionCandidate[] = (members.data ?? []).map((m) => ({
    id: m.userId,
    label: m.displayName,
  }))

  return (
    <div className={cx(styles.layout, isModal && styles.modalLayout)}>
      <div className={styles.main}>
        <div className={styles.breadcrumb}>
          <Link to="/" className={styles.crumb}>
            {projectKey}
          </Link>
          <span aria-hidden="true" className={styles.crumbSep}>
            /
          </span>
          <span className={styles.crumbKey}>{data.key}</span>
        </div>

        <div className={styles.summary}>
          <SummaryEditor cardKey={cardKey} summary={data.summary} />
        </div>

        <Description
          cardKey={cardKey}
          source={data.description ?? ''}
          candidates={candidates}
        />

        <MainTabs
          cardKey={cardKey}
          currentUserId={user?.id}
          isAdmin={user?.role === 'admin'}
          members={members.data}
        />
      </div>

      <Sidebar card={data} projectKey={projectKey} parentLookup={cardIndex.data} />
    </div>
  )
}

function SummaryEditor({ cardKey, summary }: { cardKey: string; summary: string }) {
  const patch = usePatchCard(cardKey)
  return (
    <InlineText
      value={summary}
      isHeading
      required
      label="Summary"
      placeholder="Card summary"
      onCommit={(value) => patch.mutate({ summary: value })}
    />
  )
}

function Description({
  cardKey,
  source,
  candidates,
}: {
  cardKey: string
  source: string
  candidates: MentionCandidate[]
}) {
  const patch = usePatchCard(cardKey)
  const [isEditing, setIsEditing] = useState(false)

  return (
    <section className={styles.description} aria-label="Description">
      <div className={styles.descriptionHeader}>
        <h3 className={styles.sectionTitle}>Description</h3>
        {!isEditing && (
          <Button appearance="subtle" size="compact" onClick={() => setIsEditing(true)}>
            Edit
          </Button>
        )}
      </div>

      {isEditing ? (
        <MarkdownEditor
          value={source}
          autoFocus
          isSaving={patch.isPending}
          mentionCandidates={candidates}
          onCancel={() => setIsEditing(false)}
          onSave={(markdown) => {
            patch.mutate(
              { description: markdown.trim() === '' ? null : markdown },
              { onSuccess: () => setIsEditing(false) },
            )
          }}
        />
      ) : isBlankMarkdown(source) ? (
        <button
          type="button"
          className={styles.descriptionEmpty}
          onClick={() => setIsEditing(true)}
        >
          <FileText {...ICON} aria-hidden="true" />
          Add a description…
        </button>
      ) : (
        <button
          type="button"
          className={styles.descriptionView}
          onClick={() => setIsEditing(true)}
          aria-label="Description. Click to edit."
        >
          <MarkdownView source={source} />
        </button>
      )}
      {patch.isError && !isEditing && (
        <Banner appearance="error">
          {patch.error?.problem?.detail ?? 'Could not save the description.'}
        </Banner>
      )}
    </section>
  )
}

type Tab = 'comments' | 'history'

function MainTabs({
  cardKey,
  currentUserId,
  isAdmin,
  members,
}: {
  cardKey: string
  currentUserId: string | undefined
  isAdmin: boolean
  members: Parameters<typeof Comments>[0]['members']
}) {
  const [tab, setTab] = useState<Tab>('comments')

  return (
    <div className={styles.activity}>
      <div className={styles.tabs} role="tablist" aria-label="Activity">
        <TabButton isActive={tab === 'comments'} onClick={() => setTab('comments')}>
          Comments
        </TabButton>
        <TabButton isActive={tab === 'history'} onClick={() => setTab('history')}>
          History
        </TabButton>
      </div>

      {tab === 'comments' ? (
        <div role="tabpanel">
          <Comments
            cardKey={cardKey}
            currentUserId={currentUserId}
            isAdmin={isAdmin}
            members={members}
          />
        </div>
      ) : (
        <div role="tabpanel">
          <HistoryTab cardKey={cardKey} members={members} enabled={tab === 'history'} />
        </div>
      )}
    </div>
  )
}

function TabButton({
  isActive,
  onClick,
  children,
}: {
  isActive: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={isActive}
      className={cx(styles.tab, isActive && styles.tabActive)}
      onClick={onClick}
    >
      {children}
    </button>
  )
}
