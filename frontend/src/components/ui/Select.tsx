import { ChevronDown } from 'lucide-react'
import { type ComponentPropsWithRef, useId } from 'react'

import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'

import { describedBy, Field, type FieldLabelProps } from './Field'
import styles from './Field.module.css'

export interface SelectOption {
  label: string
  value: string
  isDisabled?: boolean
}

export interface SelectProps
  extends Omit<ComponentPropsWithRef<'select'>, 'className' | 'children'>,
    FieldLabelProps {
  options: readonly SelectOption[]
  /** Renders a leading empty option — use for "no selection" states. */
  placeholder?: string
  isInvalid?: boolean
  className?: string | undefined
}

/**
 * A native <select>. ADS's own Select is react-select under the hood and is proprietary;
 * native gets correct keyboard behaviour, mobile pickers and screen-reader support for
 * free. A custom listbox (multi-select, avatars in options) can come later where it earns
 * its keep — not for every dropdown.
 */
export function Select({
  options,
  placeholder,
  isInvalid = false,
  label,
  isRequired = false,
  helpMessage,
  errorMessage,
  id,
  className,
  'aria-describedby': ariaDescribedBy,
  ...rest
}: SelectProps) {
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
      <div className={styles.selectWrap}>
        <select
          id={controlId}
          required={isRequired}
          aria-invalid={invalid || undefined}
          aria-describedby={describedBy(
            errorMessage,
            helpMessage,
            `${controlId}-error`,
            `${controlId}-help`,
            ariaDescribedBy,
          )}
          className={cx(styles.control, styles.select, invalid && styles.invalid, className)}
          {...rest}
        >
          {placeholder !== undefined && <option value="">{placeholder}</option>}
          {options.map((option) => (
            <option key={option.value} value={option.value} disabled={option.isDisabled ?? false}>
              {option.label}
            </option>
          ))}
        </select>
        <span className={styles.selectChevron}>
          <ChevronDown {...ICON} aria-hidden="true" />
        </span>
      </div>
    </Field>
  )
}
