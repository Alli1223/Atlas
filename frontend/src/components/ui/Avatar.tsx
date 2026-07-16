import { type ReactNode, useState } from 'react'

import { cx } from '@/lib/cx'

import styles from './Avatar.module.css'

export type AvatarSize = 'xsmall' | 'small' | 'medium' | 'large' | 'xlarge' | 'xxlarge'

/** Ramps used for initials fallbacks — deliberately excludes yellow/lime, whose light
 *  backgrounds carry the least contrast against the dark text at 8px. */
const INITIAL_COLORS = ['blue', 'teal', 'green', 'orange', 'red', 'magenta', 'purple'] as const

/**
 * Stable colour per name. Deterministic so a person keeps the same colour across every
 * board, session and reload — a random or index-based colour makes people unrecognisable,
 * which is the entire point of an avatar.
 */
export function avatarColor(name: string): (typeof INITIAL_COLORS)[number] {
  let hash = 0
  for (let i = 0; i < name.length; i += 1) {
    hash = (hash * 31 + name.charCodeAt(i)) | 0
  }
  return INITIAL_COLORS[Math.abs(hash) % INITIAL_COLORS.length] ?? 'blue'
}

/** "Ada Lovelace" -> "AL"; "Ada" -> "A". Ignores extra words rather than crowding. */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean)
  const first = words[0]?.[0] ?? ''
  const last = words.length > 1 ? (words[words.length - 1]?.[0] ?? '') : ''
  return (first + last).toUpperCase()
}

export interface AvatarProps {
  /** Used for the accessible name, the initials fallback and the colour hash. */
  name: string
  src?: string
  /** @default 'medium' */
  size?: AvatarSize
  /** @default 'circle' — square is for projects/issue-type tiles, circle for people. */
  appearance?: 'circle' | 'square'
  /** Adds the surface-coloured ring used when avatars overlap. */
  isStacked?: boolean
  onClick?: () => void
  className?: string | undefined
}

export function Avatar({
  name,
  src,
  size = 'medium',
  appearance = 'circle',
  isStacked = false,
  onClick,
  className,
}: AvatarProps) {
  // A broken src must fall back to initials rather than showing a torn-image icon.
  const [failed, setFailed] = useState(false)
  const showImage = src !== undefined && !failed

  const content: ReactNode = showImage ? (
    <img className={styles.image} src={src} alt="" onError={() => setFailed(true)} />
  ) : (
    <span aria-hidden="true">{initials(name)}</span>
  )

  const classes = cx(
    styles.avatar,
    styles[size],
    appearance === 'circle' ? styles.circle : styles.square,
    isStacked && styles.stacked,
    className,
  )

  const colorStyle = showImage
    ? undefined
    : {
        background: `var(--atlas-accent-${avatarColor(name)}-bg)`,
        color: `var(--atlas-accent-${avatarColor(name)}-text)`,
      }

  if (onClick !== undefined) {
    return (
      <button
        type="button"
        className={cx(styles.button, classes)}
        style={colorStyle}
        onClick={onClick}
        aria-label={name}
      >
        {content}
      </button>
    )
  }

  return (
    <span className={classes} style={colorStyle} role="img" aria-label={name} title={name}>
      {content}
    </span>
  )
}

export interface AvatarGroupProps {
  children: ReactNode
  className?: string | undefined
}

/** Overlapping row of avatars. Pass `isStacked` on each child. */
export function AvatarGroup({ children, className }: AvatarGroupProps) {
  return <span className={cx(styles.group, className)}>{children}</span>
}
