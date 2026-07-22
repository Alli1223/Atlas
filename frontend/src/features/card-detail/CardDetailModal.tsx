import { X } from 'lucide-react'
import { useEffect, useRef } from 'react'

import { Button } from '@/components/ui'
import { ICON } from '@/lib/icon'

import { CardDetail } from './CardDetail'
import styles from './CardDetailModal.module.css'

export interface CardDetailModalProps {
  /** The card to show, e.g. `ATLAS-123`. */
  cardKey: string
  /** Called when the modal should close — Escape, blanket click, or the close button. */
  onClose: () => void
}

/**
 * The card detail as an overlay — the same [`CardDetail`] the full page renders, over a
 * blanket.
 *
 * This is the *other* half of "a modal AND a full-page route": a board opens a card without
 * a navigation away, but the URL and the deep link still resolve to the full page. The two
 * share one body so they can never drift. Escape and a blanket click close it; focus moves
 * into the dialog on open and the `dialog` role + `aria-modal` announce it.
 */
export function CardDetailModal({ cardKey, onClose }: CardDetailModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    // Focus the dialog so keyboard users land inside it, not back at the board.
    dialogRef.current?.focus()
    // Lock background scroll while the overlay is up.
    const previous = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => {
      document.removeEventListener('keydown', onKeyDown)
      document.body.style.overflow = previous
    }
  }, [onClose])

  return (
    <div
      className={styles.blanket}
      // A click on the blanket itself (not its children) closes.
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose()
      }}
    >
      <div
        ref={dialogRef}
        className={styles.dialog}
        role="dialog"
        aria-modal="true"
        aria-label={`Card ${cardKey}`}
        tabIndex={-1}
      >
        <div className={styles.dialogHeader}>
          <span className={styles.dialogKey}>{cardKey}</span>
          <Button
            appearance="subtle"
            isIconOnly
            aria-label="Close"
            onClick={onClose}
            iconBefore={<X {...ICON} aria-hidden="true" />}
          />
        </div>
        <div className={styles.dialogBody}>
          <CardDetail cardKey={cardKey} isModal />
        </div>
      </div>
    </div>
  )
}
