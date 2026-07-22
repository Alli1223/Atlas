import { Avatar, Spinner } from '@/components/ui'

import type { HistoryEntry, ProjectMember } from './api'
import styles from './CardDetail.module.css'
import { formatDateTime, relativeTime } from './format'
import { useHistory } from './queries'

export interface HistoryTabProps {
  cardKey: string
  members: ProjectMember[] | undefined
  /** Deferred until the tab is shown, so opening a card never pays for history unseen. */
  enabled: boolean
}

/** Turns a camelCase/section field name into a human label: `dueDate` → `Due date`. */
function fieldLabel(field: string): string {
  const spaced = field.replace(/([a-z])([A-Z])/g, '$1 $2').replace(/[_-]+/g, ' ')
  return spaced.charAt(0).toUpperCase() + spaced.slice(1).toLowerCase()
}

/**
 * The changelog tab — every field change, who, when, from what to what.
 *
 * Reads `GET /cards/{key}/history`, which carries **display** values captured at the time of
 * the change (`fromDisplay`/`toDisplay`), not just ids — so a status renamed or a user
 * deactivated since still reads correctly, rather than showing a dangling UUID. This is the
 * whole reason `card_history` stores both raw and display (docs/adr §D1). A change that only
 * set a value (creation) shows just the new value; one that only cleared it shows the old.
 */
export function HistoryTab({ cardKey, members, enabled }: HistoryTabProps) {
  const history = useHistory(cardKey, enabled)

  if (history.isPending) {
    return (
      <div className={styles.loadingRow}>
        <Spinner size="small" label="Loading history" />
      </div>
    )
  }

  const entries = history.data ?? []
  if (entries.length === 0) {
    return <p className={styles.fieldMuted}>No changes recorded yet.</p>
  }

  // Newest first — the audit question is almost always "what just happened".
  const ordered = [...entries].reverse()

  return (
    <ol className={styles.history}>
      {ordered.map((entry) => (
        <HistoryRow
          key={entry.id}
          entry={entry}
          author={members?.find((m) => m.userId === entry.authorId)}
        />
      ))}
    </ol>
  )
}

function HistoryRow({ entry, author }: { entry: HistoryEntry; author: ProjectMember | undefined }) {
  const who = author?.displayName ?? (entry.authorId == null ? 'Automation' : 'Someone')

  return (
    <li className={styles.historyRow}>
      <Avatar name={who} size="small" />
      <div className={styles.historyBody}>
        <div className={styles.historyMeta}>
          <span className={styles.commentAuthor}>{who}</span>
          <span className={styles.commentTime} title={formatDateTime(entry.createdAt)}>
            {relativeTime(entry.createdAt)}
          </span>
        </div>
        <div className={styles.historyChange}>
          <span className={styles.historyField}>{fieldLabel(entry.field)}</span>{' '}
          {entry.fromDisplay != null && entry.fromDisplay !== '' ? (
            <>
              <span className={styles.historyFrom}>{entry.fromDisplay}</span>
              <span className={styles.historyArrow} aria-label="changed to">
                {' → '}
              </span>
            </>
          ) : (
            <span className={styles.historyArrow}>{'set to '}</span>
          )}
          {entry.toDisplay != null && entry.toDisplay !== '' ? (
            <span className={styles.historyTo}>{entry.toDisplay}</span>
          ) : (
            <span className={styles.fieldMuted}>cleared</span>
          )}
        </div>
      </div>
    </li>
  )
}
