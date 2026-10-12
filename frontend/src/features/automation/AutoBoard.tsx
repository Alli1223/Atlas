import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Banner, Button, Select } from '@/components/ui'
import { boardKeys } from '@/features/board/queries'
import { cardKeys } from '@/features/card-detail/queries'
import { cardHref } from '@/features/card-detail/autolink'
import { consoleEntry } from '@/features/card-detail/agent-console'
import type { AgentProvider } from '@/features/card-detail/api'
import { fetchAuto, startAuto, stopAuto } from './api'
import styles from './Automation.module.css'
import { AgentActivity } from './AgentActivity'

export function AutoBoard({ projectKey }: { projectKey: string }) {
  const client = useQueryClient()
  const [provider, setProvider] = useState<AgentProvider>('codex')
  const [maxTasks, setMaxTasks] = useState('3')
  const query = useQuery({ queryKey: ['board-auto', projectKey], queryFn: () => fetchAuto(projectKey),
    refetchInterval: query => ['running', 'stopping'].includes(query.state.data?.status ?? '') ? 1500 : 5000 })
  const run = query.data
  const active = run?.status === 'running' || run?.status === 'stopping'
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ['board-auto', projectKey] })
    void client.invalidateQueries({ queryKey: boardKeys.all })
  }
  const start = useMutation({ mutationFn: () => startAuto(projectKey, provider, Number(maxTasks)), onSuccess: refresh })
  const stop = useMutation({ mutationFn: () => stopAuto(projectKey), onSuccess: refresh })
  useEffect(() => {
    if (!active) return
    const timer = setInterval(() => {
      void client.invalidateQueries({ queryKey: boardKeys.all })
      void client.invalidateQueries({ queryKey: cardKeys.all })
    }, 2000)
    return () => clearInterval(timer)
  }, [active, client])
  useEffect(() => {
    if (run?.id) void client.invalidateQueries({ queryKey: boardKeys.all })
  }, [run?.status, run?.completedTasks, client, run?.id])
  return <details className={styles.panel}>
    <summary>Auto{run ? ` · ${run.status} · ${run.completedTasks}/${run.maxTasks || '∞'}` : ''}</summary>
    <p>Let a coordinator choose ready tickets and assign Codex, Claude, or Antigravity. The coordinator selects the best worker provider independently for every ticket. Workers run one at a time and leave changes for review.</p>
    <div className={styles.controls}>
      <Select label="Coordinator" value={provider} disabled={active} onChange={event => setProvider(event.target.value as AgentProvider)}
        options={[{value:'codex',label:'Codex'},{value:'claude',label:'Claude'},{value:'antigravity',label:'Antigravity'}]} />
      <Select label="Tasks per batch" value={maxTasks} disabled={active} onChange={event => setMaxTasks(event.target.value)}
        options={[...['1','3','10','25','50'].map(value => ({value,label:value})), {value:'0',label:'Until no ready tickets'}]} />
      <Button onClick={() => start.mutate()} disabled={active || query.isPending || query.isError} isLoading={start.isPending}>Auto</Button>
      {active && <Button onClick={() => stop.mutate()} disabled={run?.status === 'stopping'} isLoading={stop.isPending}>Stop Auto</Button>}
    </div>
    {query.isError && <Banner appearance="error">Could not load Auto status.</Banner>}
    {(start.error ?? stop.error) && <Banner appearance="error">{(start.error ?? stop.error)?.message}</Banner>}
    {run && <>
      <p role="status">{run.message}</p>
      {run.currentCardKey && <a href={cardHref(run.currentCardKey)}>Open {run.currentCardKey}</a>}
      <AgentActivity projectKey={projectKey} active={active} />
      {run.console && <details><summary>Coordinator console</summary><pre className={styles.console}>
        {run.console.split('\n').filter(Boolean).map(line => consoleEntry(line).text).join('\n\n')}
      </pre></details>}
    </>}
  </details>
}
