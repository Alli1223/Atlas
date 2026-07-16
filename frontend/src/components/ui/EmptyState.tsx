import { type ReactNode } from 'react'

import { cx } from '@/lib/cx'

import styles from './EmptyState.module.css'

export interface EmptyStateProps {
  header: string
  description?: ReactNode
  /** Illustration or icon. Should be decorative — the header carries the meaning. */
  image?: ReactNode
  primaryAction?: ReactNode
  secondaryAction?: ReactNode
  /** Tightens vertical padding for use inside a panel or board column. */
  isCompact?: boolean
  className?: string | undefined
}

export function EmptyState({
  header,
  description,
  image,
  primaryAction,
  secondaryAction,
  isCompact = false,
  className,
}: EmptyStateProps) {
  return (
    <div className={cx(styles.emptyState, isCompact && styles.narrow, className)}>
      {image !== undefined && (
        <div className={styles.image} aria-hidden="true">
          {image}
        </div>
      )}
      <h2 className={styles.header}>{header}</h2>
      {description !== undefined && <p className={styles.description}>{description}</p>}
      {(primaryAction !== undefined || secondaryAction !== undefined) && (
        <div className={styles.actions}>
          {primaryAction}
          {secondaryAction}
        </div>
      )}
    </div>
  )
}
