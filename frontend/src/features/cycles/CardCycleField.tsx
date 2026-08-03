import { Banner, type LozengeAppearance, Lozenge, Select, Spinner } from '@/components/ui'
import { useProject } from '@/features/projects'

import type { Cycle } from './api'
import styles from './CardCycleField.module.css'
import { useAddCardToCycle, useCardCycle, useProjectCycles, useRemoveCardFromCycle } from './queries'

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
 * The card's current cycle, and a way to add it to one or take it out.
 *
 * Mirrors `Development`'s shape: a small self-contained section that fetches its own data
 * and renders nothing when the feature does not apply — here, when the project has not
 * turned cycles on. Only `future`/`active` cycles are offered as add targets; the backend
 * refuses a closed one, so there is nothing to gain from listing it.
 */
export function CardCycleField({ cardKey, projectKey }: { cardKey: string; projectKey: string }) {
  const project = useProject(projectKey)
  const current = useCardCycle(cardKey)
  const cycles = useProjectCycles(projectKey)
  const add = useAddCardToCycle(cardKey)
  const remove = useRemoveCardFromCycle(cardKey)

  if (project.data?.cyclesEnabled !== true) return null

  const eligible = (cycles.data ?? []).filter((cycle) => cycle.state !== 'closed')

  return (
    <section className={styles.cycle} aria-labelledby={`cycle-${cardKey}`}>
      <span id={`cycle-${cardKey}`} className={styles.label}>
        Cycle
      </span>

      {current.isPending ? (
        <Spinner label="Loading cycle" />
      ) : current.data ? (
        <div className={styles.body}>
          <div className={styles.current}>
            <Lozenge appearance={STATE_APPEARANCE[current.data.state]} isBold>
              {current.data.name}
            </Lozenge>
            <button
              type="button"
              className={styles.remove}
              disabled={remove.isPending}
              onClick={() => remove.mutate()}
            >
              Remove
            </button>
          </div>
          {current.data.goal != null && current.data.goal !== '' && (
            <p className={styles.goal}>{current.data.goal}</p>
          )}
        </div>
      ) : (
        <div className={styles.body}>
          {eligible.length > 0 ? (
            <Select
              aria-label="Add to cycle"
              value=""
              placeholder={cycles.isPending ? 'Loading…' : 'Add to a cycle'}
              options={eligible.map((cycle) => ({
                label: `${cycle.name} (${STATE_LABEL[cycle.state]})`,
                value: cycle.id,
              }))}
              onChange={(event) => {
                if (event.target.value !== '') add.mutate(event.target.value)
              }}
            />
          ) : (
            <p className={styles.empty}>No open cycles.</p>
          )}
        </div>
      )}

      {add.isError && (
        <Banner appearance="error">{add.error.problem?.detail ?? 'Could not add to the cycle.'}</Banner>
      )}
      {remove.isError && (
        <Banner appearance="error">
          {remove.error.problem?.detail ?? 'Could not remove from the cycle.'}
        </Banner>
      )}
    </section>
  )
}
