import { AlertCircle, CheckCircle2, Info, X } from 'lucide-react'
import { useEffect } from 'react'

import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'

import styles from './Toaster.module.css'
import { type Toast, type ToastAppearance, useToasts } from './toast'

const ICONS: Record<ToastAppearance, typeof AlertCircle> = {
  error: AlertCircle,
  success: CheckCircle2,
  info: Info,
}

const AUTO_DISMISS_MS = 6000

function ToastRow({ toast }: { toast: Toast }) {
  const dismiss = useToasts((state) => state.dismiss)
  const Icon = ICONS[toast.appearance]

  useEffect(() => {
    const timer = setTimeout(() => dismiss(toast.id), AUTO_DISMISS_MS)
    return () => clearTimeout(timer)
  }, [toast.id, dismiss])

  return (
    <div
      className={cx(styles.toast, styles[toast.appearance])}
      role={toast.appearance === 'error' ? 'alert' : 'status'}
    >
      <span className={styles.icon}>
        <Icon {...ICON} aria-hidden="true" />
      </span>
      <span className={styles.message}>{toast.message}</span>
      <button
        type="button"
        className={styles.dismiss}
        onClick={() => dismiss(toast.id)}
        aria-label="Dismiss"
      >
        <X {...ICON} aria-hidden="true" />
      </button>
    </div>
  )
}

/** Renders the toast queue in a fixed corner region. Mount once, near the board root. */
export function Toaster() {
  const toasts = useToasts((state) => state.toasts)
  if (toasts.length === 0) return null

  return (
    <div className={styles.region} aria-live="polite">
      {toasts.map((toast) => (
        <ToastRow key={toast.id} toast={toast} />
      ))}
    </div>
  )
}
