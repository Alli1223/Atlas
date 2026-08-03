import { type FormEvent, useEffect, useRef, useState } from 'react'

import { Banner, Button, Input } from '@/components/ui'

import type { Cycle } from './api'
import dialogStyles from './CycleDialog.module.css'
import { DialogChrome } from './DialogChrome'
import { useReopenCycle } from './queries'

export interface ReopenCycleDialogProps {
  cycle: Cycle
  projectKey: string
  onClose: () => void
}

/** Reopens a `closed` cycle back to `active`, replanning its end date. */
export function ReopenCycleDialog({ cycle, projectKey, onClose }: ReopenCycleDialogProps) {
  const reopen = useReopenCycle(projectKey)
  const [endDate, setEndDate] = useState(cycle.endDate ?? '')

  const firstFieldRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    firstFieldRef.current?.focus()
  }, [])

  const canSubmit = endDate !== '' && !reopen.isPending

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (!canSubmit) return
    reopen.mutate({ id: cycle.id, endDate }, { onSuccess: () => onClose() })
  }

  return (
    <DialogChrome title={`Reopen “${cycle.name}”`} onClose={onClose}>
      <form className={dialogStyles.form} onSubmit={onSubmit}>
        {reopen.isError && (
          <Banner appearance="error">
            {reopen.error.problem?.detail ?? 'Could not reopen the cycle.'}
          </Banner>
        )}

        <p className={dialogStyles.note}>
          The start date ({cycle.startDate}) stays as it was — only the end date is replanned.
          Cards carried away when this cycle closed are not automatically restored.
        </p>

        <Input
          ref={firstFieldRef}
          type="date"
          label="New end date"
          isRequired
          value={endDate}
          onChange={(event) => setEndDate(event.target.value)}
        />

        <div className={dialogStyles.actions}>
          <Button appearance="subtle" onClick={onClose} type="button">
            Cancel
          </Button>
          <Button
            appearance="primary"
            type="submit"
            isLoading={reopen.isPending}
            disabled={!canSubmit}
          >
            Reopen cycle
          </Button>
        </div>
      </form>
    </DialogChrome>
  )
}
