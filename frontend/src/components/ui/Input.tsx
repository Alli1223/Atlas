import { type ComponentPropsWithRef, useId } from 'react'

import { cx } from '@/lib/cx'

import { describedBy, Field, type FieldLabelProps } from './Field'
import styles from './Field.module.css'

export interface InputProps
  extends Omit<ComponentPropsWithRef<'input'>, 'size' | 'className'>,
    FieldLabelProps {
  /** 32px default, 24px compact. @default 'default' */
  size?: 'default' | 'compact'
  /** Forces the invalid styling. `errorMessage` implies it. */
  isInvalid?: boolean
  className?: string | undefined
}

export function Input({
  size = 'default',
  isInvalid = false,
  label,
  isRequired = false,
  helpMessage,
  errorMessage,
  id,
  className,
  type = 'text',
  'aria-describedby': ariaDescribedBy,
  ...rest
}: InputProps) {
  const generatedId = useId()
  const controlId = id ?? generatedId
  const invalid = isInvalid || errorMessage !== undefined

  return (
    <Field
      label={label}
      isRequired={isRequired}
      helpMessage={helpMessage}
      errorMessage={errorMessage}
      controlId={controlId}
      helpId={`${controlId}-help`}
      errorId={`${controlId}-error`}
    >
      <input
        id={controlId}
        type={type}
        required={isRequired}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy(
          errorMessage,
          helpMessage,
          `${controlId}-error`,
          `${controlId}-help`,
          ariaDescribedBy,
        )}
        className={cx(
          styles.control,
          styles.input,
          size === 'compact' && styles.compact,
          invalid && styles.invalid,
          className,
        )}
        {...rest}
      />
    </Field>
  )
}
