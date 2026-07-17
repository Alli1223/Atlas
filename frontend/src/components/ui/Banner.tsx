import { AlertTriangle, Info, XCircle } from 'lucide-react'
import { type ReactNode } from 'react'

import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'

import styles from './Banner.module.css'

export type BannerAppearance = 'announcement' | 'warning' | 'error'

const ICONS = {
  announcement: Info,
  warning: AlertTriangle,
  error: XCircle,
} as const

export interface BannerProps {
  children: ReactNode
  /** @default 'announcement' */
  appearance?: BannerAppearance
  /** Overrides the default icon. Pass `null` for no icon. */
  icon?: ReactNode | null
  /** Trailing controls — e.g. a "Renew" or "Dismiss" Button. */
  actions?: ReactNode
  className?: string | undefined
}

/**
 * Full-width status bar. Errors and warnings announce themselves to assistive tech:
 * `alert` interrupts, `status` waits for a pause — which is the right split, since a
 * failed agent run or an expired PAT is worth interrupting for and an announcement is not.
 */
export function Banner({
  children,
  appearance = 'announcement',
  icon,
  actions,
  className,
}: BannerProps) {
  const Icon = ICONS[appearance]

  return (
    <div
      className={cx(styles.banner, styles[appearance], className)}
      role={appearance === 'error' ? 'alert' : 'status'}
      // Surfaces the variant in the DOM. `role` cannot identify a banner on its own — a
      // Spinner is also role=status — so this is the stable hook for styling and assertions
      // that does not depend on hashed CSS-module class names.
      data-appearance={appearance}
    >
      {icon !== null && (
        <span className={styles.icon}>
          {icon ?? <Icon {...ICON} aria-hidden="true" />}
        </span>
      )}
      <span className={styles.content}>{children}</span>
      {actions !== undefined && <span className={styles.actions}>{actions}</span>}
    </div>
  )
}
