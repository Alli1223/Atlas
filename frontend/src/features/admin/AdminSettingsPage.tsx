import { AlertTriangle, CheckCircle, Cpu, Database, Download, HardDrive } from 'lucide-react'
import { useState } from 'react'

import { Button, Lozenge, Spinner } from '@/components/ui'
import { ICON } from '@/lib/icon'

import styles from './AdminSettingsPage.module.css'
import { useApplyUpdate, useSystemStats, useUpdateStatus } from './queries'

// ── helpers ──────────────────────────────────────────────────────────────────

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const k = 1024
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${units[i]}`
}

function UsageBar({ used, total, label }: { used: number; total: number; label: string }) {
  const pct = total > 0 ? Math.min(100, (used / total) * 100) : 0
  const critical = pct >= 90
  const warning = pct >= 70

  return (
    <div className={styles.usageBar}>
      <div className={styles.usageBarTrack}>
        <div
          className={styles.usageBarFill}
          style={{ width: `${pct.toFixed(1)}%` }}
          data-critical={critical}
          data-warning={!critical && warning}
          role="progressbar"
          aria-valuenow={Math.round(pct)}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label={label}
        />
      </div>
      <span className={styles.usageBarLabel}>{pct.toFixed(1)}%</span>
    </div>
  )
}

// ── system stats card ─────────────────────────────────────────────────────────

function SystemStatsCard() {
  const { data, isPending, isError } = useSystemStats()

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>System resources</h2>
      <p className={styles.cardDescription}>Live host metrics. Refreshes every 10 seconds.</p>

      {isPending && (
        <div className={styles.center}>
          <Spinner />
        </div>
      )}

      {isError && (
        <p className={styles.errorText}>Could not load system stats. Check server logs.</p>
      )}

      {data && (
        <dl className={styles.statGrid}>
          <div className={styles.statRow}>
            <dt className={styles.statLabel}>
              <Cpu {...ICON} aria-hidden="true" size={16} />
              CPU usage
            </dt>
            <dd className={styles.statValue}>
              <UsageBar used={data.cpuUsagePercent} total={100} label="CPU usage" />
              <span className={styles.statDetail}>{data.cpuUsagePercent.toFixed(1)}% across all cores</span>
            </dd>
          </div>

          <div className={styles.statRow}>
            <dt className={styles.statLabel}>
              <Database {...ICON} aria-hidden="true" size={16} />
              Memory
            </dt>
            <dd className={styles.statValue}>
              <UsageBar used={data.memoryUsedBytes} total={data.memoryTotalBytes} label="Memory usage" />
              <span className={styles.statDetail}>
                {formatBytes(data.memoryUsedBytes)} / {formatBytes(data.memoryTotalBytes)}
              </span>
            </dd>
          </div>

          <div className={styles.statRow}>
            <dt className={styles.statLabel}>
              <HardDrive {...ICON} aria-hidden="true" size={16} />
              Disk
            </dt>
            <dd className={styles.statValue}>
              <UsageBar used={data.diskUsedBytes} total={data.diskTotalBytes} label="Disk usage" />
              <span className={styles.statDetail}>
                {formatBytes(data.diskUsedBytes)} used · {formatBytes(data.diskTotalBytes - data.diskUsedBytes)} free
              </span>
            </dd>
          </div>
        </dl>
      )}
    </section>
  )
}

// ── updates card ──────────────────────────────────────────────────────────────

function UpdatesCard() {
  const { data, isPending, isError, refetch } = useUpdateStatus()
  const applyUpdate = useApplyUpdate()
  const [applied, setApplied] = useState(false)

  async function handleApply() {
    await applyUpdate.mutateAsync()
    setApplied(true)
  }

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Software updates</h2>
      <p className={styles.cardDescription}>
        Polls GitHub Releases for a newer version of Atlas.
      </p>

      {isPending && (
        <div className={styles.center}>
          <Spinner />
        </div>
      )}

      {isError && (
        <p className={styles.errorText}>Could not check for updates.</p>
      )}

      {data && (
        <div className={styles.updateBody}>
          <div className={styles.versionRow}>
            <div className={styles.versionItem}>
              <span className={styles.versionLabel}>Running</span>
              <code className={styles.versionCode}>v{data.currentVersion}</code>
            </div>
            {data.latestVersion && (
              <div className={styles.versionItem}>
                <span className={styles.versionLabel}>Latest</span>
                <code className={styles.versionCode}>v{data.latestVersion}</code>
              </div>
            )}
            <div className={styles.versionItem}>
              <span className={styles.versionLabel}>Status</span>
              {data.error ? (
                <Lozenge appearance="removed">Check failed</Lozenge>
              ) : data.hasUpdate ? (
                <Lozenge appearance="new">Update available</Lozenge>
              ) : (
                <Lozenge appearance="success">Up to date</Lozenge>
              )}
            </div>
          </div>

          {data.error && (
            <div className={styles.notice} data-kind="warning">
              <AlertTriangle size={16} aria-hidden="true" />
              {data.error}
            </div>
          )}

          {!data.error && !data.hasUpdate && (
            <div className={styles.notice} data-kind="success">
              <CheckCircle size={16} aria-hidden="true" />
              Atlas is running the latest release.
            </div>
          )}

          {applied && (
            <div className={styles.notice} data-kind="success">
              <CheckCircle size={16} aria-hidden="true" />
              Update queued. Atlas will rebuild and restart in a few minutes.
              Follow progress with: <code>journalctl -fu atlas-update</code>
            </div>
          )}

          {data.releaseNotes && (
            <details className={styles.releaseNotes}>
              <summary>Release notes — v{data.latestVersion ?? data.currentVersion}</summary>
              <pre className={styles.releaseBody}>{data.releaseNotes}</pre>
            </details>
          )}

          <div className={styles.updateActions}>
            <Button appearance="subtle" size="compact" onClick={() => void refetch()}>
              Check again
            </Button>
            {data.releaseUrl && (
              <a
                href={data.releaseUrl}
                target="_blank"
                rel="noreferrer"
                className={styles.releaseLink}
              >
                View release
              </a>
            )}
            {data.hasUpdate && !applied && (
              <Button
                appearance="primary"
                size="compact"
                iconBefore={<Download {...ICON} aria-hidden="true" size={16} />}
                onClick={() => void handleApply()}
                disabled={applyUpdate.isPending}
              >
                {applyUpdate.isPending ? 'Queuing…' : 'Update now'}
              </Button>
            )}
          </div>
        </div>
      )}
    </section>
  )
}

// ── page ──────────────────────────────────────────────────────────────────────

export function AdminSettingsPage() {
  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div>
          <h1>Administrator settings</h1>
          <p className={styles.lede}>Instance health and maintenance.</p>
        </div>
      </header>

      <SystemStatsCard />
      <UpdatesCard />
    </div>
  )
}
