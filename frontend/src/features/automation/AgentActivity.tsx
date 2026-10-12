import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Activity, Bot, Terminal } from 'lucide-react'
import { fetchAgentTranscript, fetchCardAgentSessions } from '@/features/card-detail/api'
import { consoleEntry } from '@/features/card-detail/agent-console'
import { cardHref } from '@/features/card-detail/autolink'
import { fetchAutoActivity, type AgentActivity as ActivityItem } from './api'
import styles from './Automation.module.css'

function WorkerConsole({ event }: { event: ActivityItem }) {
  const transcript = useQuery({ queryKey: ['agent-transcript', event.sessionId],
    queryFn: () => fetchAgentTranscript(event.sessionId!), enabled: !!event.sessionId,
    refetchInterval: 2000 })
  const sessions = useQuery({ queryKey: ['auto-worker-status', event.cardKey],
    queryFn: () => fetchCardAgentSessions(event.cardKey!), enabled: !!event.cardKey, refetchInterval: 2000 })
  const session = sessions.data?.find(session => session.id === event.sessionId)
  return <div>
    <p>Run status: <strong>{session?.status ?? 'Loading…'}</strong></p>
    {session?.errorMessage && <p role="alert">{session.errorMessage}</p>}
    {transcript.isError && <p role="alert">Could not load worker output.</p>}
    <div className={styles.output} role="log" aria-label={`${event.provider} output for ${event.cardKey}`}>
      {transcript.data?.map(entry => {
        const formatted = consoleEntry(entry.line)
        return <pre key={entry.id} className={formatted.attention ? styles.attention : undefined}>{formatted.text}</pre>
      })}
      {!transcript.data?.length && <p>Waiting for agent output…</p>}
    </div>
  </div>
}

export function AgentActivity({ projectKey, active }: { projectKey: string; active: boolean }) {
  const query = useQuery({ queryKey: ['auto-activity', projectKey], queryFn: () => fetchAutoActivity(projectKey),
    refetchInterval: active ? 1500 : 5000 })
  return <section className={styles.activity} aria-label="Agent activity">
    <h3><Activity size={18} aria-hidden="true" /> Agent activity</h3>
    {query.isError && <p role="alert">Could not load agent activity.</p>}
    {!query.isError && !query.data?.length && <p>Start Auto to see planning, assignments, and worker progress here.</p>}
    <ol className={styles.timeline}>
      {query.data?.map(event => <li key={event.id} className={styles.event}>
        <div className={styles.eventIcon}><Bot size={18} aria-hidden="true" /></div>
        <div className={styles.eventBody}>
          <div className={styles.eventHeading}>
            <span className={styles.provider}>{event.provider}</span>
            <strong>{event.cardKey ? <a href={cardHref(event.cardKey)}>{event.cardKey}</a> : 'Coordinator'}</strong>
            <time dateTime={event.createdAt}>{new Date(event.createdAt).toLocaleTimeString()}</time>
          </div>
          <p>{event.message}</p>
          {event.sessionId && <ConsoleDisclosure event={event} />}
        </div>
      </li>)}
    </ol>
  </section>
}

function ConsoleDisclosure({ event }: { event: ActivityItem }) {
  const [open, setOpen] = useState(false)
  return <details onToggle={toggle => setOpen(toggle.currentTarget.open)}>
    <summary><Terminal size={14} aria-hidden="true" /> Worker console</summary>
    {open && <WorkerConsole event={event} />}
  </details>
}
