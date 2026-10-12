import { Bot, X } from 'lucide-react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'

import { Banner, Button, Lozenge, type LozengeAppearance, Select, Spinner } from '@/components/ui'
import { ICON, ICON_SMALL } from '@/lib/icon'

import type { AgentProvider, AgentSession, AgentSessionStatus, Card } from './api'
import { fetchAgentTranscript } from './api'
import styles from './AgentSessions.module.css'
import { formatDateTime } from './format'
import { consoleEntry } from './agent-console'
import { cardKeys, useCancelAgentSession, useCardAgentSessions, useStartAgentSession } from './queries'

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

const PROVIDER_LABEL: Record<AgentProvider, string> = {
  claude: 'Claude', codex: 'Codex', antigravity: 'Antigravity',
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
  const [provider, setProvider] = useState<AgentProvider>('claude')
  const sessions = useCardAgentSessions(card.key)
  const start = useStartAgentSession(card.key)
  const cancel = useCancelAgentSession(card.key)

  const latest = sessions.data?.[0]
  const isRunning = latest?.status === 'running'

  return (
    <section className={styles.agentSessions} aria-labelledby={`agent-${card.key}`}>
      <span id={`agent-${card.key}`} className={styles.label}>
        Coding agent
      </span>

      <div className={styles.body}>
        <Select label="Agent" value={provider} disabled={isRunning || start.isPending}
          options={Object.entries(PROVIDER_LABEL).map(([value, label]) => ({value, label}))}
          onChange={(event) => setProvider(event.target.value as AgentProvider)} />
        <Button
          appearance="default"
          size="compact"
          iconBefore={<Bot {...ICON} aria-hidden="true" />}
          isLoading={start.isPending}
          disabled={isRunning}
          onClick={() => start.mutate(provider)}
        >
          Run with {PROVIDER_LABEL[provider]}
        </Button>

        {start.isError && (
          <Banner appearance="error">
            {start.error.problem?.detail ?? 'Could not start the run.'}
          </Banner>
        )}
        {cancel.isError && (
          <Banner appearance="error">
            {cancel.error.problem?.detail ?? 'Could not cancel the run.'}
          </Banner>
        )}

        {sessions.isError ? (
          <Banner appearance="error">Could not load agent sessions.</Banner>
        ) : sessions.isPending ? (
          <Spinner label="Loading sessions" />
        ) : sessions.data && sessions.data.length > 0 ? (
          <ul className={styles.sessions}>
            {sessions.data.map((session) => (
              <SessionRow
                key={session.id}
                session={session}
                cardKey={card.key}
                onCancel={() => cancel.mutate(session.id)}
                isCancelling={cancel.isPending && cancel.variables === session.id}
              />
            ))}
          </ul>
        ) : (
          <p className={styles.empty}>No runs yet.</p>
        )}
      </div>
    </section>
  )
}

function SessionRow({
  session,
  cardKey,
  onCancel,
  isCancelling,
}: {
  session: AgentSession
  cardKey: string
  onCancel: () => void
  isCancelling: boolean
}) {
  return (
    <li className={styles.session}>
      <div className={styles.sessionHeader}>
        <Lozenge appearance={STATUS_APPEARANCE[session.status]} isBold>
          {STATUS_LABEL[session.status]}
        </Lozenge>
        <span className={styles.meta}>{PROVIDER_LABEL[session.provider as AgentProvider] ?? 'Claude'}</span>
        <span className={styles.started} title={formatDateTime(session.startedAt)}>
          {formatDateTime(session.startedAt)}
        </span>
        {session.status === 'running' && (
          <Button
            className={styles.cancelButton}
            appearance="subtle"
            size="compact"
            isIconOnly
            aria-label="Cancel run"
            iconBefore={<X {...ICON_SMALL} aria-hidden="true" />}
            isLoading={isCancelling}
            onClick={onCancel}
          />
        )}
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
      <SessionTranscript session={session} cardKey={cardKey} />
    </li>
  )
}

function SessionTranscript({ session, cardKey }: { session: AgentSession; cardKey: string }) {
  const queryClient = useQueryClient()
  const running = session.status === 'running'
  const [expanded, setExpanded] = useState(false)
  const [raw, setRaw] = useState(false)
  const open = running || expanded
  const transcript = useQuery({
    queryKey: ['agent-transcript', session.id],
    queryFn: () => fetchAgentTranscript(session.id),
    enabled: open,
    refetchInterval: running ? 1500 : false,
    staleTime: 0,
  })
  useEffect(() => {
    if (!running) void queryClient.invalidateQueries({ queryKey: ['agent-transcript', session.id] })
  }, [running, session.id, queryClient])
  const entries = transcript.data?.map(entry => consoleEntry(entry.line)) ?? []
  const requests = entries.filter(entry => entry.attention)
  const requestCount = requests.length
  useEffect(() => {
    if (requestCount > 0) void queryClient.invalidateQueries({ queryKey: cardKeys.comments(cardKey) })
  }, [requestCount, queryClient, cardKey])
  return (
    <details open={open} onToggle={(event) => { if (!running) setExpanded(event.currentTarget.open) }}>
      <summary>{running ? 'Live console output' : 'View transcript'}</summary>
      {requests.length > 0 && <Banner appearance="warning">
        <strong>Agent needs attention</strong>
        {requests.map((request, index) => <pre className={styles.request} key={index}>{request.text}</pre>)}
        <p>This CLI runs without interactive input. For a question, add your answer to the ticket description,
          then rerun after completion. Cancel first if it is still running. Review denied permissions before rerunning.</p>
      </Banner>}
      {open && <>
        <p className={styles.meta}>{running ? 'Updates every 1.5 seconds while this ticket is open.' : 'Recorded console output.'}</p>
        <label className={styles.meta}><input type="checkbox" checked={raw} onChange={event => setRaw(event.target.checked)} /> Show raw events</label>
        {transcript.isError ? <Banner appearance="error">Could not load the transcript. Retrying while the run is active.</Banner>
          : transcript.isPending ? <Spinner label="Loading transcript" />
          : <pre className={styles.transcript} aria-label="Agent console" tabIndex={0}>{
            (raw ? transcript.data.map(entry => entry.line) : entries.map(entry => entry.text).filter(Boolean)).join('\n\n')
            || (running ? 'Waiting for agent output…' : 'No transcript recorded.')
          }</pre>}
      </>}
    </details>
  )
}
