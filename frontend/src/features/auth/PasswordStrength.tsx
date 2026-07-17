import { Check, Circle } from 'lucide-react'

import { cx } from '@/lib/cx'
import { ICON_SMALL } from '@/lib/icon'

import type { PasswordAssessment } from './password'
import styles from './PasswordStrength.module.css'

export interface PasswordStrengthProps {
  assessment: PasswordAssessment
  /** Hides the meter until the user has typed something. @default false */
  isEmpty?: boolean
}

const SEGMENTS = [1, 2, 3, 4] as const

/**
 * The strength meter and the live rule checklist.
 *
 * # Why the rules are `aria-live`
 *
 * A sighted user watches the ticks turn green as they type. Without a live region, a screen
 * reader user gets nothing until submit — the whole point of live feedback is that it
 * arrives before you commit, and "before you commit" has to mean the same thing for
 * everybody. `polite` rather than `assertive`: this is progress, not an emergency, and
 * interrupting every keystroke would be unusable.
 */
export function PasswordStrength({ assessment, isEmpty = false }: PasswordStrengthProps) {
  const { score, label } = assessment.strength

  return (
    <div className={styles.wrap}>
      <div
        className={styles.meter}
        // The meter is a summary of the rules below, and the rules are already announced.
        // Announcing both would say everything twice.
        aria-hidden="true"
      >
        {SEGMENTS.map((segment) => (
          <span
            key={segment}
            className={cx(styles.segment, segment <= score && styles.filled)}
            data-score={segment <= score ? score : undefined}
          />
        ))}
      </div>

      <span className={styles.label} aria-hidden="true">
        {!isEmpty && `Password strength: ${label}`}
      </span>

      <ul className={styles.rules} aria-live="polite">
        {assessment.rules.map((rule) => (
          <li key={rule.id} className={cx(styles.rule, rule.satisfied && styles.satisfied)}>
            <span className={styles.icon}>
              {rule.satisfied ? (
                <Check {...ICON_SMALL} aria-hidden="true" />
              ) : (
                <Circle {...ICON_SMALL} aria-hidden="true" />
              )}
            </span>
            {/* The state is in the text, not only in the icon and the colour: a live region
                announces the text content, and "At least 12 characters" alone would read
                identically whether it passed or failed. */}
            <span data-rule={rule.id} data-satisfied={rule.satisfied}>
              {rule.label}
            </span>
            <span className="visually-hidden">{rule.satisfied ? ' — done' : ' — not yet'}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}
