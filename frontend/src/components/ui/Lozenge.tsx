import { type ComponentPropsWithRef } from 'react'

import { cx } from '@/lib/cx'

import styles from './Lozenge.module.css'

export type LozengeAppearance = 'default' | 'inprogress' | 'success' | 'removed' | 'new' | 'moved'

/**
 * Atlas has exactly three status categories, like Jira — boards, reports and AQL all key
 * off these three buckets, so the set is deliberately closed.
 */
export type StatusCategory = 'todo' | 'inprogress' | 'done'

/**
 * The canonical mapping, from @atlaskit/lozenge's own legacyAppearanceMap:
 * To Do -> neutral/grey, In Progress -> information/BLUE, Done -> success/LIME.
 *
 * Note "green" is really lime in the brand-refresh palette — --ds-background-success
 * resolves to the Lime ramp. Reaching for Green here is the single easiest way to look
 * subtly wrong.
 */
export const STATUS_CATEGORY_APPEARANCE: Record<StatusCategory, LozengeAppearance> = {
  todo: 'default',
  inprogress: 'inprogress',
  done: 'success',
}

export interface LozengeProps extends Omit<ComponentPropsWithRef<'span'>, 'className'> {
  /** @default 'default' */
  appearance?: LozengeAppearance
  /** Convenience over `appearance` for card/board status. Wins if both are passed. */
  statusCategory?: StatusCategory
  isBold?: boolean
  className?: string | undefined
}

export function Lozenge({
  appearance = 'default',
  statusCategory,
  isBold = false,
  children,
  className,
  ...rest
}: LozengeProps) {
  const resolved = statusCategory !== undefined ? STATUS_CATEGORY_APPEARANCE[statusCategory] : appearance

  return (
    <span
      className={cx(styles.lozenge, styles[resolved], isBold && styles.bold, className)}
      {...rest}
    >
      {children}
    </span>
  )
}
