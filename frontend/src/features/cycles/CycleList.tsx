import { Plus } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Banner, Button, EmptyState, Input, Lozenge, type LozengeAppearance, Spinner } from '@/components/ui'
import { ICON } from '@/lib/icon'

import type { Cycle } from './api'
import { CompleteCycleDialog } from './CompleteCycleDialog'
import styles from './CycleList.module.css'
import { ReopenCycleDialog } from './ReopenCycleDialog'
import { useCreateCycle, useProjectCycles, useUpdateCycle } from './queries'
import { StartCycleDialog } from './StartCycleDialog'

const STATE_APPEARANCE: Record<Cycle['state'], LozengeAppearance> = {
  future: 'default',
  active: 'inprogress',
  closed: 'success',
}

const STATE_LABEL: Record<Cycle['state'], string> = {
  future: 'Future',
  active: 'Active',
  closed: 'Closed',
}

/**
 * A project's cycles: create one, rename it or edit its goal, and drive it through
 * `future -> active -> closed`, with reopening back to `active`.
 *
 * Card membership itself is not managed here — that is one card at a time, from the card
 * detail sidebar's `CardCycleField`. A dedicated backlog board (dragging cards between the
 * plain backlog and a cycle) is future work; this is the cycle *lifecycle* view.
 */
export function CycleList({ projectKey }: { projectKey: string }) {
  const cycles = useProjectCycles(projectKey)
  const [creating, setCreating] = useState(false)

  if (cycles.isPending) {
    return (
      <div className={styles.state}>
        <Spinner size="large" />
      </div>
    )
  }

  const all = cycles.data ?? []
  const active = all.filter((cycle) => cycle.state === 'active')
  const future = all.filter((cycle) => cycle.state === 'future')
  const closed = all.filter((cycle) => cycle.state === 'closed')
  const openCycles = [...active, ...future]

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Cycles</h2>
        <Button
          appearance="primary"
          size="compact"
          iconBefore={<Plus {...ICON} aria-hidden="true" />}
          onClick={() => setCreating(true)}
        >
          New cycle
        </Button>
      </div>

      {creating && (
        <CreateCycleForm projectKey={projectKey} onDone={() => setCreating(false)} />
      )}

      {all.length === 0 && !creating ? (
        <EmptyState
          header="No cycles yet"
          description="Create a cycle to start planning sprints for this project."
        />
      ) : (
        <>
          <CycleGroup
            title="Active"
            cycles={active}
            projectKey={projectKey}
            openCycles={openCycles}
          />
          <CycleGroup
            title="Future"
            cycles={future}
            projectKey={projectKey}
            openCycles={openCycles}
          />
          <CycleGroup
            title="Closed"
            cycles={closed}
            projectKey={projectKey}
            openCycles={openCycles}
          />
        </>
      )}
    </div>
  )
}

function CreateCycleForm({
  projectKey,
  onDone,
}: {
  projectKey: string
  onDone: () => void
}) {
  const create = useCreateCycle(projectKey)
  const [name, setName] = useState('')
  const [goal, setGoal] = useState('')

  function onSubmit(event: FormEvent) {
    event.preventDefault()
    if (name.trim() === '' || create.isPending) return
    const trimmedGoal = goal.trim()
    create.mutate(
      { name: name.trim(), ...(trimmedGoal !== '' && { goal: trimmedGoal }) },
      { onSuccess: () => onDone() },
    )
  }

  return (
    <form className={styles.createForm} onSubmit={onSubmit}>
      {create.isError && (
        <Banner appearance="error">
          {create.error.problem?.detail ?? 'Could not create the cycle.'}
        </Banner>
      )}
      <Input
        label="Name"
        isRequired
        autoFocus
        value={name}
        onChange={(event) => setName(event.target.value)}
        placeholder="e.g. Sprint 14"
      />
      <Input
        label="Goal"
        value={goal}
        onChange={(event) => setGoal(event.target.value)}
        placeholder="Optional"
      />
      <div className={styles.formActions}>
        <Button appearance="subtle" type="button" onClick={onDone}>
          Cancel
        </Button>
        <Button
          appearance="primary"
          type="submit"
          isLoading={create.isPending}
          disabled={name.trim() === ''}
        >
          Create
        </Button>
      </div>
    </form>
  )
}

function CycleGroup({
  title,
  cycles,
  projectKey,
  openCycles,
}: {
  title: string
  cycles: Cycle[]
  projectKey: string
  openCycles: Cycle[]
}) {
  if (cycles.length === 0) return null

  return (
    <section className={styles.group} aria-label={title}>
      <h3 className={styles.groupTitle}>{title}</h3>
      <ul className={styles.list}>
        {cycles.map((cycle) => (
          <CycleRow
            key={cycle.id}
            cycle={cycle}
            projectKey={projectKey}
            otherOpenCycles={openCycles.filter((other) => other.id !== cycle.id)}
          />
        ))}
      </ul>
    </section>
  )
}

type DialogKind = 'start' | 'complete' | 'reopen' | null

function CycleRow({
  cycle,
  projectKey,
  otherOpenCycles,
}: {
  cycle: Cycle
  projectKey: string
  otherOpenCycles: Cycle[]
}) {
  const update = useUpdateCycle(projectKey)
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState(cycle.name)
  const [goal, setGoal] = useState(cycle.goal ?? '')
  const [dialog, setDialog] = useState<DialogKind>(null)

  function onSave(event: FormEvent) {
    event.preventDefault()
    if (name.trim() === '' || update.isPending) return
    const trimmedGoal = goal.trim()
    update.mutate(
      {
        id: cycle.id,
        name: name.trim(),
        goal: trimmedGoal === '' ? null : trimmedGoal,
      },
      { onSuccess: () => setEditing(false) },
    )
  }

  return (
    <li className={styles.row}>
      {editing ? (
        <form className={styles.editForm} onSubmit={onSave}>
          {update.isError && (
            <Banner appearance="error">
              {update.error.problem?.detail ?? 'Could not update the cycle.'}
            </Banner>
          )}
          <Input
            label="Name"
            isRequired
            autoFocus
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <Input
            label="Goal"
            value={goal}
            onChange={(event) => setGoal(event.target.value)}
            placeholder="Optional"
          />
          <div className={styles.formActions}>
            <Button
              appearance="subtle"
              type="button"
              onClick={() => {
                setName(cycle.name)
                setGoal(cycle.goal ?? '')
                setEditing(false)
              }}
            >
              Cancel
            </Button>
            <Button
              appearance="primary"
              type="submit"
              isLoading={update.isPending}
              disabled={name.trim() === ''}
            >
              Save
            </Button>
          </div>
        </form>
      ) : (
        <>
          <div className={styles.rowMain}>
            <div className={styles.rowHeading}>
              <Lozenge appearance={STATE_APPEARANCE[cycle.state]} isBold>
                {STATE_LABEL[cycle.state]}
              </Lozenge>
              <span className={styles.name}>{cycle.name}</span>
            </div>
            {cycle.goal != null && cycle.goal !== '' && (
              <p className={styles.goal}>{cycle.goal}</p>
            )}
            {cycle.startDate != null && cycle.endDate != null && (
              <p className={styles.dates}>
                {cycle.startDate} – {cycle.endDate}
              </p>
            )}
          </div>

          <div className={styles.rowActions}>
            <Button appearance="subtle" size="compact" onClick={() => setEditing(true)}>
              Edit
            </Button>
            {cycle.state === 'future' && (
              <Button appearance="default" size="compact" onClick={() => setDialog('start')}>
                Start
              </Button>
            )}
            {cycle.state === 'active' && (
              <Button appearance="default" size="compact" onClick={() => setDialog('complete')}>
                Complete
              </Button>
            )}
            {cycle.state === 'closed' && (
              <Button appearance="default" size="compact" onClick={() => setDialog('reopen')}>
                Reopen
              </Button>
            )}
          </div>
        </>
      )}

      {dialog === 'start' && (
        <StartCycleDialog cycle={cycle} projectKey={projectKey} onClose={() => setDialog(null)} />
      )}
      {dialog === 'complete' && (
        <CompleteCycleDialog
          cycle={cycle}
          projectKey={projectKey}
          otherOpenCycles={otherOpenCycles}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog === 'reopen' && (
        <ReopenCycleDialog cycle={cycle} projectKey={projectKey} onClose={() => setDialog(null)} />
      )}
    </li>
  )
}
