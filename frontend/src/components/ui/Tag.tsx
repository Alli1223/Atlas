import { X } from 'lucide-react'
import { type ReactNode } from 'react'

import { cx } from '@/lib/cx'
import { ICON_SMALL } from '@/lib/icon'

import styles from './Tag.module.css'

export const TAG_COLORS = [
  'standard',
  'grey',
  'blue',
  'teal',
  'green',
  'lime',
  'yellow',
  'orange',
  'red',
  'magenta',
  'purple',
] as const

export type TagColor = (typeof TAG_COLORS)[number]

export interface TagProps {
  children: ReactNode
  /** @default 'standard' */
  color?: TagColor
  /** Pill shape. @default false */
  isRounded?: boolean
  /** Turns the chip into a link — e.g. "filter this board by this tag". */
  href?: string
  /** Renders a remove affordance. The label is built from `removeButtonLabel`. */
  onRemove?: () => void
  /** @default `Remove` */
  removeButtonLabel?: string
  className?: string | undefined
}

export function Tag({
  children,
  color = 'standard',
  isRounded = false,
  href,
  onRemove,
  removeButtonLabel,
  className,
}: TagProps) {
  const text =
    href !== undefined ? (
      <a className={cx(styles.text, styles.link)} href={href}>
        {children}
      </a>
    ) : (
      <span className={styles.text}>{children}</span>
    )

  return (
    <span className={cx(styles.tag, styles[color], isRounded && styles.rounded, className)}>
      {text}
      {onRemove !== undefined && (
        <button
          type="button"
          className={styles.remove}
          onClick={onRemove}
          aria-label={removeButtonLabel ?? `Remove ${typeof children === 'string' ? children : 'tag'}`}
        >
          <X {...ICON_SMALL} aria-hidden="true" />
        </button>
      )}
    </span>
  )
}
