import { useState } from 'react'

import { Avatar, Button, Spinner } from '@/components/ui'

import type { Comment, ProjectMember } from './api'
import styles from './CardDetail.module.css'
import { relativeTime, formatDateTime } from './format'
import { MarkdownEditor } from './MarkdownEditor'
import { MarkdownView } from './MarkdownView'
import { useAddComment, useComments, useDeleteComment, useEditComment } from './queries'
import type { MentionCandidate } from './mention-suggestion'

export interface CommentsProps {
  cardKey: string
  /** The signed-in user's id, to decide which comments are editable/deletable. */
  currentUserId: string | undefined
  /** Whether the signed-in user is an instance admin (may delete any comment). */
  isAdmin: boolean
  members: ProjectMember[] | undefined
}

/**
 * The comment thread: a list, an editor to add, and edit/delete on your own.
 *
 * Comments are markdown all the way down — stored as source, rendered through
 * [`MarkdownView`] (the sanitisation boundary), edited through the same TipTap surface as
 * the description. Edit and delete are offered only where the backend will allow them (own
 * comment, or admin-delete), so the UI never dangles an action that 403s.
 */
export function Comments({ cardKey, currentUserId, isAdmin, members }: CommentsProps) {
  const comments = useComments(cardKey)
  const add = useAddComment(cardKey)

  const candidates: MentionCandidate[] = (members ?? []).map((m) => ({
    id: m.userId,
    label: m.displayName,
  }))

  // A key that changes on each successful post resets the editor to empty.
  const [composerKey, setComposerKey] = useState(0)

  return (
    <section className={styles.comments} aria-label="Comments">
      <h3 className={styles.sectionTitle}>Comments</h3>

      <div className={styles.composer}>
        <MarkdownEditor
          key={composerKey}
          value=""
          placeholder="Add a comment…"
          saveLabel="Comment"
          isSaving={add.isPending}
          mentionCandidates={candidates}
          onSave={(markdown) => {
            if (markdown.trim() === '') return
            add.mutate(markdown, { onSuccess: () => setComposerKey((k) => k + 1) })
          }}
        />
      </div>

      {comments.isPending ? (
        <div className={styles.loadingRow}>
          <Spinner size="small" label="Loading comments" />
        </div>
      ) : comments.data && comments.data.length > 0 ? (
        <ol className={styles.commentList}>
          {comments.data.map((comment) => (
            <CommentRow
              key={comment.id}
              cardKey={cardKey}
              comment={comment}
              author={members?.find((m) => m.userId === comment.authorId)}
              canEdit={comment.authorId === currentUserId}
              canDelete={comment.authorId === currentUserId || isAdmin}
              candidates={candidates}
            />
          ))}
        </ol>
      ) : (
        <p className={styles.fieldMuted}>No comments yet.</p>
      )}
    </section>
  )
}

function CommentRow({
  cardKey,
  comment,
  author,
  canEdit,
  canDelete,
  candidates,
}: {
  cardKey: string
  comment: Comment
  author: ProjectMember | undefined
  canEdit: boolean
  canDelete: boolean
  candidates: MentionCandidate[]
}) {
  const edit = useEditComment(cardKey)
  const remove = useDeleteComment(cardKey)
  const [isEditing, setIsEditing] = useState(false)

  const name = author?.displayName ?? 'Unknown user'

  return (
    <li className={styles.commentRow}>
      <Avatar name={name} size="small" />
      <div className={styles.commentBody}>
        <div className={styles.commentMeta}>
          <span className={styles.commentAuthor}>{name}</span>
          <span className={styles.commentTime} title={formatDateTime(comment.createdAt)}>
            {relativeTime(comment.createdAt)}
          </span>
          {comment.editedAt && <span className={styles.commentEdited}>(edited)</span>}
        </div>

        {isEditing ? (
          <MarkdownEditor
            value={comment.body}
            autoFocus
            isSaving={edit.isPending}
            mentionCandidates={candidates}
            onCancel={() => setIsEditing(false)}
            onSave={(markdown) => {
              if (markdown.trim() === '') return
              edit.mutate(
                { id: comment.id, body: markdown },
                { onSuccess: () => setIsEditing(false) },
              )
            }}
          />
        ) : (
          <>
            <MarkdownView source={comment.body} />
            {(canEdit || canDelete) && (
              <div className={styles.commentActions}>
                {canEdit && (
                  <Button appearance="subtle" size="compact" onClick={() => setIsEditing(true)}>
                    Edit
                  </Button>
                )}
                {canDelete && (
                  <Button
                    appearance="subtle"
                    size="compact"
                    isLoading={remove.isPending}
                    onClick={() => {
                      if (window.confirm('Delete this comment?')) remove.mutate(comment.id)
                    }}
                  >
                    Delete
                  </Button>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </li>
  )
}
