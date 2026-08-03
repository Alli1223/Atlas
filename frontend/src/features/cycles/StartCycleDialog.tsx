import { type FormEvent, useEffect, useRef, useState } from 'react'

import { Banner, Button, Input } from '@/components/ui'

import type { Cycle } from './api'
import dialogStyles from './CycleDialog.module.css'
import { DialogChrome } from './DialogChrome'
import { useStartCycle } from './queries'

export interface StartCycleDialogProps {
  cycle: Cycle
  projectKey: string
  onClose: () => void
}

/** Starts a `future` cycle: collects both dates the state machine requires together. */
export function StartCycleDialog({ cycle, projectKey, onClose }: StartCycleDialogProps) {
  const start = useStartCycle(projectKey)
  const [startDate, setStartDate] = useState('')
  const [endDate, setEndDate] = useState('')

  const firstFieldRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    firstFieldRef.current?.focus()
  }, [])

  const canSubmit = startDate !== '' && endDate !== '' && !start.isPending

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (!canSubmit) return
    start.mutate({ id: cycle.id, startDate, endDate }, { onSuccess: () => onClose() })
  }

  return (
    <DialogChrome title={`Start “${cycle.name}”`} onClose={onClose}>
      <form className={dialogStyles.form} onSubmit={onSubmit}>
        {start.isError && (
          <Banner appearance="error">
            {start.error.problem?.detail ?? 'Could not start the cycle.'}
          </Banner>
        )}

        <Input
          ref={firstFieldRef}
          type="date"
          label="Start date"
          isRequired
          value={startDate}
          onChange={(event) => setStartDate(event.target.value)}
        />
        <Input
          type="date"
          label="End date"
          isRequired
          value={endDate}
          onChange={(event) => setEndDate(event.target.value)}
        />

        <div className={dialogStyles.actions}>
          <Button appearance="subtle" onClick={onClose} type="button">
            Cancel
          </Button>
          <Button appearance="primary" type="submit" isLoading={start.isPending} disabled={!canSubmit}>
            Start cycle
          </Button>
        </div>
      </form>
    </DialogChrome>
  )
}
