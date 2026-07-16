import { type ReactNode } from 'react'

import styles from './Field.module.css'

export interface FieldLabelProps {
  /** Omit for controls labelled by something else (e.g. a toolbar with an aria-label). */
  label?: ReactNode
  isRequired?: boolean
  /** Persistent hint. Hidden while `errorMessage` is showing, so the two never stack. */
  helpMessage?: ReactNode
  errorMessage?: ReactNode
}

export interface FieldProps extends FieldLabelProps {
  controlId: string
  helpId: string
  errorId: string
  children: ReactNode
}

/**
 * Label + help/error scaffolding shared by Input, Textarea and Select.
 *
 * The wiring (`aria-describedby`, `aria-invalid`, `htmlFor`) is the entire point:
 * getting it right once here means no field in Atlas can ship it wrong.
 */
export function Field({
  label,
  isRequired = false,
  helpMessage,
  errorMessage,
  controlId,
  helpId,
  errorId,
  children,
}: FieldProps) {
  return (
    <div className={styles.field}>
      {label !== undefined && (
        <label className={styles.label} htmlFor={controlId}>
          {label}
          {isRequired && (
            <span className={styles.required} aria-hidden="true">
              *
            </span>
          )}
        </label>
      )}
      {children}
      {errorMessage === undefined && helpMessage !== undefined && (
        <span className={styles.message} id={helpId}>
          {helpMessage}
        </span>
      )}
      {errorMessage !== undefined && (
        <span className={styles.errorMessage} id={errorId}>
          {errorMessage}
        </span>
      )}
    </div>
  )
}

/** Computes the describedby list for a control given which messages are present. */
export function describedBy(
  errorMessage: ReactNode | undefined,
  helpMessage: ReactNode | undefined,
  errorId: string,
  helpId: string,
  own: string | undefined,
): string | undefined {
  const ids = [
    own,
    errorMessage !== undefined ? errorId : undefined,
    errorMessage === undefined && helpMessage !== undefined ? helpId : undefined,
  ].filter(Boolean)
  return ids.length > 0 ? ids.join(' ') : undefined
}
