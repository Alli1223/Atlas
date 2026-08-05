import { Bot } from 'lucide-react'

import { Banner, Button, Lozenge, type LozengeAppearance, Spinner } from '@/components/ui'
import { ICON } from '@/lib/icon'

import type { AgentSession, AgentSessionStatus, Card } from './api'
import styles from './AgentSessions.module.css'
import { formatDateTime } from './format'
import { useCardAgentSessions, useStartAgentSession } from './queries'

const STATUS_APPEARANCE: Record<AgentSessionStatus, LozengeAppearance> = {
  running: 'inprogress',
  completed: 'success',
  completed_with_denials: 'new',
  limit_reached: 'removed',
  failed: 'removed',
  cancelled: 'default',
}

const STATUS_LABEL: Record<AgentSessionStatus, string> = {
  running: 'Running',
  completed: 'Completed',
  completed_with_denials: 'Completed (some tools denied)',
  limit_reached: 'Limit reached',
  failed: 'Failed',
  cancelled: 'Cancelled',
}

/**
 * "Run with Claude": starts a Claude Code session against the card (prompt = summary +
 * description, built server-side) and shows the card's session history.
 *
 * The list is the whole state machine here — there is no separate "current run" concept in
 * the UI beyond "the newest item in the list", which `useCardAgentSessions` also polls while
 * it is `running`. A card with no linked repo still gets a working button: starting a run is
 * what surfaces that conflict (`agent::workspace::prepare` refuses with no repo linked), not
 * something this panel needs to pre-check.
 */
export function AgentSessions({ card }: { card: Card }) {
  const sessions = useCardAgentSessions(card.key)
  const start = useStartAgentSession(card.key)

  const latest = sessions.data?.[0]
  const isRunning = latest?.status === 'running'

  return (
    <section className={styles.agentSessions} aria-labelledby={`agent-${card.key}`}>
      <span id={`agent-${card.key}`} className={styles.label}>
        Claude Code
      </span>

      <div className={styles.body}>
        <Button
          appearance="default"
          size="compact"
          iconBefore={<Bot {...ICON} aria-hidden="true" />}
          isLoading={start.isPending}
          disabled={isRunning}
          onClick={() => start.mutate()}
        >
          Run with Claude
        </Button>

        {start.isError && (
          <Banner appearance="error">
            {start.error.problem?.detail ?? 'Could not start the run.'}
          </Banner>
        )}

        {sessions.isPending ? (
          <Spinner label="Loading sessions" />
        ) : sessions.data && sessions.data.length > 0 ? (
          <ul className={styles.sessions}>
            {sessions.data.map((session) => (
              <SessionRow key={session.id} session={session} />
            ))}
          </ul>
        ) : (
          <p className={styles.empty}>No runs yet.</p>
        )}
      </div>
    </section>
  )
}

function SessionRow({ session }: { session: AgentSession }) {
  return (
    <li className={styles.session}>
      <div className={styles.sessionHeader}>
        <Lozenge appearance={STATUS_APPEARANCE[session.status]} isBold>
          {STATUS_LABEL[session.status]}
        </Lozenge>
        <span className={styles.started} title={formatDateTime(session.startedAt)}>
          {formatDateTime(session.startedAt)}
        </span>
      </div>
      {session.totalCostUsd != null && (
        <span className={styles.meta}>
          ${session.totalCostUsd.toFixed(2)}
          {session.numTurns != null &&
            ` · ${session.numTurns} turn${session.numTurns === 1 ? '' : 's'}`}
        </span>
      )}
      {session.errorMessage && <p className={styles.error}>{session.errorMessage}</p>}
      {session.resultText && <p className={styles.result}>{session.resultText}</p>}
    </li>
  )
}
