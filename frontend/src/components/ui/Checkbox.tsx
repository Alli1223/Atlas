import { type ComponentPropsWithRef, type ReactNode, useEffect, useRef } from 'react'

import { cx } from '@/lib/cx'

import styles from './Choice.module.css'

export interface CheckboxProps extends Omit<ComponentPropsWithRef<'input'>, 'type' | 'className'> {
  label?: ReactNode
  /** Tri-state. `indeterminate` is a DOM property, not an attribute — set via ref below. */
  isIndeterminate?: boolean
  isInvalid?: boolean
  className?: string | undefined
}

export function Checkbox({
  label,
  isIndeterminate = false,
  isInvalid = false,
  className,
  ref,
  ...rest
}: CheckboxProps) {
  const innerRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (innerRef.current) {
      innerRef.current.indeterminate = isIndeterminate
    }
  }, [isIndeterminate])

  const input = (
    <input
      type="checkbox"
      ref={(node) => {
        innerRef.current = node
        if (typeof ref === 'function') {
          return ref(node)
        }
        if (ref) {
          ref.current = node
        }
        return undefined
      }}
      aria-invalid={isInvalid || undefined}
      className={cx(styles.input, styles.checkbox, isInvalid && styles.invalid, className)}
      {...rest}
    />
  )

  if (label === undefined) {
    return input
  }

  return (
    <label className={styles.wrapper}>
      {input}
      <span className={styles.label}>{label}</span>
    </label>
  )
}
