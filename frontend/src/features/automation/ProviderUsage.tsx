import { useQuery } from '@tanstack/react-query'
import { Gauge } from 'lucide-react'
import { fetchProviderUsage } from './api'
import styles from './Automation.module.css'

const number = (value: number | null | undefined) => value == null ? 'Not reported' : value.toLocaleString()
export function ProviderUsage() {
  const query = useQuery({ queryKey: ['provider-usage'], queryFn: fetchProviderUsage, refetchInterval: 15000 })
  const total = query.data?.reduce((sum, usage) => sum + usage.runs, 0) ?? 0
  const active = query.data?.reduce((sum, usage) => sum + usage.activeRuns, 0) ?? 0
  return <details className={styles.usage}>
    <summary><Gauge size={16} aria-hidden="true" /> AI usage <span>{active > 0 ? `${active} active · ` : ''}{total} calls</span></summary>
    <div className={styles.usagePopover}>
      <strong>Your Atlas usage · last 24 hours</strong>
      <p>Finished CLI calls, including Auto planning. Updates every 15 seconds.</p>
      {query.isError ? <p role="alert">Could not load usage.</p> : query.isPending ? <p>Loading usage…</p> :
        <div className={styles.usageGrid}>{['codex', 'claude', 'antigravity'].map(provider => {
          const usage = query.data?.find(item => item.provider === provider)
          return <section key={provider}>
            <strong className={styles.provider}>{provider}</strong>
            <dl>
              <dt>Active agents</dt><dd>{usage?.activeRuns ?? 0}</dd>
              <dt>Finished calls</dt><dd>{usage?.runs ?? 0}</dd>
              <dt>Input tokens</dt><dd>{number(usage?.inputTokens)}</dd>
              <dt>Output tokens</dt><dd>{number(usage?.outputTokens)}</dd>
              <dt>Cached input</dt><dd>{number(usage?.cachedTokens)}</dd>
              <dt>Reported cost</dt><dd>{usage?.costUsd == null ? 'Not reported' : `$${usage.costUsd.toFixed(4)}`}</dd>
            </dl>
            {!!usage && usage.measuredRuns < usage.runs && <small>Tokens reported for {usage.measuredRuns}/{usage.runs} calls.</small>}
          </section>
        })}</div>}
      <p>CLI estimates, not your subscription allowance or billing balance. Unreported usage stays unknown.</p>
    </div>
  </details>
}
