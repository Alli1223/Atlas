import { type ReactNode } from 'react'

import { Banner } from '@/components/ui'

import styles from './AuthScreen.module.css'

export interface AuthScreenProps {
  /** The card's heading. */
  title: string
  /** A sentence under the heading explaining what this screen wants and why. */
  lede?: ReactNode
  /** Shown in an error Banner above the form. Omit for none. */
  error?: ReactNode
  /** Small print under the card. */
  footer?: ReactNode
  children: ReactNode
}

/**
 * The signed-out screen chrome: logo, one centred card, nothing else.
 *
 * Shared by login and change-password so the two are the same object moving rather than
 * two screens that happen to look similar — the forced-reset flow puts them back to back,
 * where any drift between them is immediately visible.
 */
export function AuthScreen({ title, lede, error, footer, children }: AuthScreenProps) {
  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <div className={styles.brand}>
          {/* Decorative: the wordmark next to it already says "Atlas", and a screen reader
              announcing it twice is noise. */}
          <img src="/atlas.svg" alt="" className={styles.brandMark} />
          <span className={styles.brandName}>Atlas</span>
        </div>

        <div className={styles.card}>
          <header className={styles.header}>
            <h1 className={styles.title}>{title}</h1>
            {lede !== undefined && <p className={styles.lede}>{lede}</p>}
          </header>

          {/* Banner's error appearance carries role="alert", so a failure that arrives after
              submit is announced without stealing focus from the field being corrected. */}
          {error !== undefined && (
            <Banner appearance="error" className={styles.banner}>
              {error}
            </Banner>
          )}

          {children}
        </div>

        {footer !== undefined && <p className={styles.footer}>{footer}</p>}
      </div>
    </div>
  )
}
