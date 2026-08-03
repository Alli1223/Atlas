import { X } from 'lucide-react'
import { type ReactNode, useEffect, useId } from 'react'

import { Button } from '@/components/ui'
import { ICON } from '@/lib/icon'

import styles from './CycleDialog.module.css'

/**
 * The hand-built modal overlay shared by the three cycle-action dialogs — the same shape as
 * `LinkRepoDialog` (there is no shared Modal primitive project-wide), pulled out once here
 * because three dialogs would otherwise repeat it identically. Escape closes it; focusing
 * the first field is each dialog's own concern, since only it knows which field that is.
 */
export function DialogChrome({
  title,
  onClose,
  children,
}: {
  title: string
  onClose: () => void
  children: ReactNode
}) {
  const titleId = useId()

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [onClose])

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
            {title}
          </h2>
          <Button
            appearance="subtle"
            isIconOnly
            aria-label="Close"
            onClick={onClose}
            iconBefore={<X {...ICON} aria-hidden="true" />}
          />
        </header>
        {children}
      </div>
    </div>
  )
}
