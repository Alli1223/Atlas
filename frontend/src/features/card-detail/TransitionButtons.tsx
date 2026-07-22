import { Button, Banner } from '@/components/ui'

import type { AvailableTransition } from './api'
import styles from './CardDetail.module.css'
import { useExecuteTransition, usePatchCard, useTransitions } from './queries'

export interface TransitionButtonsProps {
  cardKey: string
}

/**
 * The legal moves a card can make right now, as buttons.
 *
 * # The hide-vs-reject contract, from the client side
 *
 * `GET /cards/{key}/transitions` has already run every transition's **conditions** and
 * dropped the ones that fail — so this component renders *exactly* the list it is given and
 * never invents a move. That is the whole point of the endpoint: Jira's transition UI never
 * shows a button you cannot press, because a condition that fails hides the button rather
 * than greying it out (see `crate::domain::workflow`). Validators are the other half —
 * those transitions *are* shown, and a failure comes back as a 422 when pressed, surfaced
 * here as a banner rather than a hidden button.
 *
 * A transition with a `null` id is the permissive default workflow's implicit move; it has
 * no gates to run, so it is taken as a plain status edit (which still fires the resolution
 * rules server-side) rather than through the transition executor.
 */
export function TransitionButtons({ cardKey }: TransitionButtonsProps) {
  const transitions = useTransitions(cardKey)
  const execute = useExecuteTransition(cardKey)
  const patch = usePatchCard(cardKey)

  const pending = execute.isPending || patch.isPending
  const error = execute.error ?? patch.error

  function take(transition: AvailableTransition) {
    if (transition.id != null) {
      execute.mutate({ transitionId: transition.id })
    } else {
      patch.mutate({ statusId: transition.toStatusId })
    }
  }

  if (transitions.isPending) return null

  const moves = transitions.data ?? []
  if (moves.length === 0) {
    return <p className={styles.noMoves}>No moves available from here.</p>
  }

  return (
    <div className={styles.transitions}>
      <div className={styles.transitionRow} role="group" aria-label="Move this card">
        {moves.map((transition) => (
          <Button
            // `id` can be null for the default move, so key on name+target, which is unique
            // within one card's available set.
            key={transition.id ?? `default-${transition.toStatusId}`}
            appearance="default"
            size="compact"
            isLoading={pending}
            onClick={() => take(transition)}
          >
            {transition.name}
          </Button>
        ))}
      </div>
      {error && (
        <Banner appearance="error">
          {error.problem?.detail ?? 'That move was rejected. Refresh and try again.'}
        </Banner>
      )}
    </div>
  )
}
