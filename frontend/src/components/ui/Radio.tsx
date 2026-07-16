import { type ComponentPropsWithRef, type ReactNode } from 'react'

import { cx } from '@/lib/cx'

import styles from './Choice.module.css'

export interface RadioProps extends Omit<ComponentPropsWithRef<'input'>, 'type' | 'className'> {
  label?: ReactNode
  isInvalid?: boolean
  className?: string | undefined
}

export function Radio({ label, isInvalid = false, className, ...rest }: RadioProps) {
  const input = (
    <input
      type="radio"
      aria-invalid={isInvalid || undefined}
      className={cx(styles.input, styles.radio, isInvalid && styles.invalid, className)}
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

export interface RadioGroupProps {
  /** Renders a <fieldset>/<legend>, which is how a radio set gets a group name. */
  label?: ReactNode
  name: string
  options: readonly { label: string; value: string; isDisabled?: boolean }[]
  value?: string
  defaultValue?: string
  onChange?: (value: string) => void
  isDisabled?: boolean
}

export function RadioGroup({
  label,
  name,
  options,
  value,
  defaultValue,
  onChange,
  isDisabled = false,
}: RadioGroupProps) {
  return (
    <fieldset className={styles.group}>
      {label !== undefined && <legend className={styles.groupLabel}>{label}</legend>}
      {options.map((option) => (
        <Radio
          key={option.value}
          name={name}
          value={option.value}
          label={option.label}
          disabled={isDisabled || (option.isDisabled ?? false)}
          {...(value !== undefined
            ? { checked: value === option.value }
            : { defaultChecked: defaultValue === option.value })}
          onChange={(event) => {
            if (event.currentTarget.checked) {
              onChange?.(option.value)
            }
          }}
        />
      ))}
    </fieldset>
  )
}
