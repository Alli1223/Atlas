import { type FormEvent, useState } from 'react'

import { Avatar, Button, Input, Spinner } from '@/components/ui'
import { ApiError } from '@/lib/api'

import type { ProjectMember, Worklog as WorklogEntry } from './api'
import styles from './CardDetail.module.css'
import { formatDateTime, formatMinutes, relativeTime } from './format'
import { useLogTime, useWorklogs } from './queries'
import worklogStyles from './Worklog.module.css'

export interface WorklogProps {
  cardKey: string
  members: ProjectMember[] | undefined
}

/**
 * The worklog tab: a duration/note composer, and the resulting log — newest first, with the
 * running total in the section heading.
 *
 * The manual counterpart to a smart commit's `#time 2h` directive
 * (`crate::integrations::github::smart_commit`); both write the same append-only
 * `card_worklogs` rows, which is why a smart-commit entry shows up here too, tagged by
 * source rather than hidden.
 */
export function Worklog({ cardKey, members }: WorklogProps) {
  const worklogs = useWorklogs(cardKey)
  const logTime = useLogTime(cardKey)

  const [duration, setDuration] = useState('')
  const [note, setNote] = useState('')

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (duration.trim() === '') return
    logTime.mutate(
      { duration: duration.trim(), note: note.trim() },
      {
        onSuccess: () => {
          setDuration('')
          setNote('')
        },
      },
    )
  }

  const errorMessage =
    logTime.isError && logTime.error instanceof ApiError
      ? (logTime.error.problem?.detail ?? 'Could not log that time.')
      : undefined

  return (
    <section className={styles.comments} aria-label="Time logged">
      <h3 className={styles.sectionTitle}>
        Time logged
        {worklogs.data && worklogs.data.totalMinutes > 0
          ? ` — ${formatMinutes(worklogs.data.totalMinutes)}`
          : ''}
      </h3>

      <form className={worklogStyles.composer} onSubmit={onSubmit}>
        <div className={worklogStyles.durationField}>
          <Input
            label="Duration"
            placeholder="2h 30m"
            value={duration}
            onChange={(event) => setDuration(event.target.value)}
            size="compact"
            {...(errorMessage !== undefined && { errorMessage })}
          />
        </div>
        <div className={worklogStyles.noteField}>
          <Input
            label="Note"
            placeholder="Optional note"
            value={note}
            onChange={(event) => setNote(event.target.value)}
            size="compact"
          />
        </div>
        <Button
          type="submit"
          appearance="primary"
          size="compact"
          isLoading={logTime.isPending}
          disabled={duration.trim() === ''}
        >
          Log time
        </Button>
      </form>

      {worklogs.isPending ? (
        <div className={styles.loadingRow}>
          <Spinner size="small" label="Loading worklogs" />
        </div>
      ) : worklogs.data && worklogs.data.entries.length > 0 ? (
        <ol className={styles.commentList}>
          {worklogs.data.entries.map((entry) => (
            <WorklogRow
              key={entry.id}
              entry={entry}
              author={members?.find((m) => m.userId === entry.authorId)}
            />
          ))}
        </ol>
      ) : (
        <p className={styles.fieldMuted}>No time logged yet.</p>
      )}
    </section>
  )
}

function WorklogRow({
  entry,
  author,
}: {
  entry: WorklogEntry
  author: ProjectMember | undefined
}) {
  const name = author?.displayName ?? 'Unknown user'
  return (
    <li className={styles.commentRow}>
      <Avatar name={name} size="small" />
      <div className={styles.commentBody}>
        <div className={styles.commentMeta}>
          <span className={styles.commentAuthor}>{name}</span>
          <span className={worklogStyles.duration}>{formatMinutes(entry.minutes)}</span>
          <span className={styles.commentTime} title={formatDateTime(entry.createdAt)}>
            {relativeTime(entry.createdAt)}
          </span>
          {entry.source === 'smart-commit' && (
            <span className={worklogStyles.source}>smart commit</span>
          )}
        </div>
        {entry.note && <p className={worklogStyles.note}>{entry.note}</p>}
      </div>
    </li>
  )
}
