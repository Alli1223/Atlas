import { type ComponentPropsWithRef, useId } from 'react'

import { cx } from '@/lib/cx'

import { describedBy, Field, type FieldLabelProps } from './Field'
import styles from './Field.module.css'

export interface TextareaProps
  extends Omit<ComponentPropsWithRef<'textarea'>, 'className'>,
    FieldLabelProps {
  isInvalid?: boolean
  className?: string | undefined
}

export function Textarea({
  isInvalid = false,
  label,
  isRequired = false,
  helpMessage,
  errorMessage,
  id,
  className,
  rows = 3,
  'aria-describedby': ariaDescribedBy,
  ...rest
}: TextareaProps) {
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
      <textarea
        id={controlId}
        rows={rows}
        required={isRequired}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy(
          errorMessage,
          helpMessage,
          `${controlId}-error`,
          `${controlId}-help`,
          ariaDescribedBy,
        )}
        className={cx(styles.control, styles.textarea, invalid && styles.invalid, className)}
        {...rest}
      />
    </Field>
  )
}
