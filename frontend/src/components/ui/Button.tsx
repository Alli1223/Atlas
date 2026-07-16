import { type ComponentPropsWithRef, type ReactNode } from 'react'

import { cx } from '@/lib/cx'

import styles from './Button.module.css'
import { Spinner } from './Spinner'

export type ButtonAppearance = 'default' | 'primary' | 'subtle' | 'link' | 'danger' | 'warning'
export type ButtonSize = 'default' | 'compact'

export interface ButtonProps extends Omit<ComponentPropsWithRef<'button'>, 'className'> {
  /** @default 'default' */
  appearance?: ButtonAppearance
  /** 32px default, 24px compact — both from @atlaskit/button. @default 'default' */
  size?: ButtonSize
  /**
   * Shows a spinner and blocks interaction. The label stays in flow (hidden) so the
   * button does not resize under the pointer mid-click.
   */
  isLoading?: boolean
  iconBefore?: ReactNode
  iconAfter?: ReactNode
  /** Renders a square icon-only button. Requires `aria-label`. */
  isIconOnly?: boolean
  shouldFitContainer?: boolean
  className?: string | undefined
}

export function Button({
  appearance = 'default',
  size = 'default',
  isLoading = false,
  iconBefore,
  iconAfter,
  isIconOnly = false,
  shouldFitContainer = false,
  disabled,
  children,
  className,
  type = 'button',
  ...rest
}: ButtonProps) {
  const isDisabled = disabled === true || isLoading

  return (
    <button
      type={type}
      disabled={isDisabled}
      aria-busy={isLoading || undefined}
      className={cx(
        styles.button,
        styles[appearance],
        size === 'compact' && styles.compact,
        isIconOnly && styles.iconOnly,
        shouldFitContainer && styles.fullWidth,
        isLoading && styles.loading,
        className,
      )}
      {...rest}
    >
      {isLoading && (
        <span className={styles.spinnerSlot}>
          {/* label={null}: aria-busy on the button already announces the state. */}
          <Spinner size={size === 'compact' ? 'xsmall' : 'small'} label={null} />
        </span>
      )}
      <span className={cx(styles.contents, isLoading && styles.loadingLabel)}>
        {iconBefore !== undefined && <span className={styles.icon}>{iconBefore}</span>}
        {children}
        {iconAfter !== undefined && <span className={styles.icon}>{iconAfter}</span>}
      </span>
    </button>
  )
}
