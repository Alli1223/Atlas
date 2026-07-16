import { type ComponentPropsWithRef } from 'react'

import { cx } from '@/lib/cx'

import styles from './Spinner.module.css'

/** ADS spinner sizes. */
const SIZES = {
  xsmall: 12,
  small: 16,
  medium: 24,
  large: 48,
  xlarge: 96,
} as const

export type SpinnerSize = keyof typeof SIZES

export interface SpinnerProps extends Omit<ComponentPropsWithRef<'svg'>, 'width' | 'height'> {
  /** @default 'medium' */
  size?: SpinnerSize
  /**
   * Accessible name. Pass `null` when the spinner sits inside an element that already
   * announces the busy state (e.g. a Button with aria-busy), to avoid a double
   * announcement.
   * @default 'Loading'
   */
  label?: string | null
}

export function Spinner({ size = 'medium', label = 'Loading', className, ...rest }: SpinnerProps) {
  const px = SIZES[size]
  // Stroke thins as the spinner grows, matching ADS's optical weight across sizes.
  const strokeWidth = px <= 16 ? 2 : px <= 24 ? 2.5 : 3
  const radius = (32 - strokeWidth) / 2

  return (
    <svg
      viewBox="0 0 32 32"
      width={px}
      height={px}
      fill="none"
      className={cx(styles.spinner, className)}
      // Exempt from the global prefers-reduced-motion freeze: a still spinner no longer
      // communicates "busy", which is a regression rather than an accommodation.
      data-preserve-motion=""
      role={label === null ? 'presentation' : 'status'}
      aria-label={label ?? undefined}
      aria-hidden={label === null ? true : undefined}
      {...rest}
    >
      <circle className={styles.track} cx="16" cy="16" r={radius} strokeWidth={strokeWidth} />
      <circle className={styles.head} cx="16" cy="16" r={radius} strokeWidth={strokeWidth} />
    </svg>
  )
}
