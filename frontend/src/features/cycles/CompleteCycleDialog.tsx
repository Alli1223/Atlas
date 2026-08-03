import { type FormEvent, useEffect, useRef, useState } from 'react'

import { Banner, Button, Input, RadioGroup, Select } from '@/components/ui'

import type { CarryTo, Cycle } from './api'
import dialogStyles from './CycleDialog.module.css'
import { DialogChrome } from './DialogChrome'
import { useCompleteCycle } from './queries'

export interface CompleteCycleDialogProps {
  cycle: Cycle
  projectKey: string
  /** The project's other open cycles — candidates for "carry into an existing cycle". */
  otherOpenCycles: Cycle[]
  onClose: () => void
}

const CARRY_OPTIONS = [
  { label: 'Move to the backlog', value: 'backlog' },
  { label: 'Move to an existing cycle', value: 'existingCycle' },
  { label: 'Move to a new cycle', value: 'newCycle' },
]

/**
 * Completes an `active` cycle: any card still not Done leaves it, and this collects where
 * those cards go — mirroring `domain::cycle::CarryTo` exactly.
 */
export function CompleteCycleDialog({
  cycle,
  projectKey,
  otherOpenCycles,
  onClose,
}: CompleteCycleDialogProps) {
  const complete = useCompleteCycle(projectKey)
  const [carryKind, setCarryKind] = useState<CarryTo['kind']>('backlog')
  const [existingCycleId, setExistingCycleId] = useState(otherOpenCycles[0]?.id ?? '')
  const [newCycleName, setNewCycleName] = useState('')

  const firstFieldRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    firstFieldRef.current?.focus()
  }, [])

  const carryTo: CarryTo | undefined =
    carryKind === 'backlog'
      ? { kind: 'backlog' }
      : carryKind === 'existingCycle'
        ? existingCycleId !== ''
          ? { kind: 'existingCycle', cycleId: existingCycleId }
          : undefined
        : newCycleName.trim() !== ''
          ? { kind: 'newCycle', name: newCycleName.trim() }
          : undefined

  const canSubmit = carryTo !== undefined && !complete.isPending

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (carryTo === undefined || !canSubmit) return
    complete.mutate({ id: cycle.id, carryTo }, { onSuccess: () => onClose() })
  }

  return (
    <DialogChrome title={`Complete “${cycle.name}”`} onClose={onClose}>
      <form className={dialogStyles.form} onSubmit={onSubmit}>
        {complete.isError && (
          <Banner appearance="error">
            {complete.error.problem?.detail ?? 'Could not complete the cycle.'}
          </Banner>
        )}

        <p className={dialogStyles.note}>
          Cards not yet Done leave this cycle. Where should they go?
        </p>

        <RadioGroup
          label="Incomplete cards"
          name="carry-to"
          value={carryKind}
          options={
            otherOpenCycles.length > 0
              ? CARRY_OPTIONS
              : CARRY_OPTIONS.filter((option) => option.value !== 'existingCycle')
          }
          onChange={(value) => setCarryKind(value as CarryTo['kind'])}
        />

        {carryKind === 'existingCycle' && (
          <Select
            label="Target cycle"
            value={existingCycleId}
            onChange={(event) => setExistingCycleId(event.target.value)}
            options={otherOpenCycles.map((option) => ({ label: option.name, value: option.id }))}
          />
        )}

        {carryKind === 'newCycle' && (
          <Input
            ref={firstFieldRef}
            label="New cycle name"
            isRequired
            value={newCycleName}
            onChange={(event) => setNewCycleName(event.target.value)}
            placeholder="e.g. Sprint 15"
          />
        )}

        <div className={dialogStyles.actions}>
          <Button appearance="subtle" onClick={onClose} type="button">
            Cancel
          </Button>
          <Button
            appearance="primary"
            type="submit"
            isLoading={complete.isPending}
            disabled={!canSubmit}
          >
            Complete cycle
          </Button>
        </div>
      </form>
    </DialogChrome>
  )
}
