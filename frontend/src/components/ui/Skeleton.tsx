import { cx } from '@/lib/cx'

import styles from './Skeleton.module.css'

export interface SkeletonProps {
  /** Any CSS length. @default '100%' */
  width?: string | number
  /** Any CSS length. @default '1em' */
  height?: string | number
  /** For avatar/icon placeholders. */
  isCircle?: boolean
  /** @default true */
  hasShimmer?: boolean
  className?: string | undefined
}

/**
 * A loading placeholder. Always `aria-hidden`: the surrounding region should own the
 * busy state (`aria-busy`), otherwise every skeleton line announces itself and a loading
 * board becomes a wall of noise for a screen-reader user.
 */
export function Skeleton({
  width = '100%',
  height = '1em',
  isCircle = false,
  hasShimmer = true,
  className,
}: SkeletonProps) {
  return (
    <span
      className={cx(styles.skeleton, isCircle && styles.circle, hasShimmer && styles.shimmer, className)}
      style={{ width, height }}
      aria-hidden="true"
    />
  )
}

export interface SkeletonTextProps {
  /** @default 3 */
  lines?: number
  className?: string | undefined
}

/** Paragraph placeholder. The last line is short, which is what sells it as text. */
export function SkeletonText({ lines = 3, className }: SkeletonTextProps) {
  return (
    <span className={className} aria-hidden="true">
      {Array.from({ length: lines }, (_, i) => (
        <Skeleton
          key={i}
          className={styles.text}
          height={12}
          width={i === lines - 1 && lines > 1 ? '60%' : '100%'}
        />
      ))}
    </span>
  )
}
