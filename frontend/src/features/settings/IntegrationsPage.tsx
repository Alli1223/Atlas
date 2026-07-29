import { PlugZap } from 'lucide-react'

import { Banner, Skeleton } from '@/components/ui'
import { ApiError } from '@/lib/api'

import { IntegrationsBanner } from './IntegrationsBanner'
import styles from './IntegrationsPage.module.css'
import { ProviderSection } from './ProviderSection'
import { useCredentials } from './queries'
import { groupByProvider, PROVIDERS } from './status'

/**
 * Settings → Integrations: manage the API keys for every provider Atlas talks to.
 *
 * The page is deliberately calm — a trustworthy place to keep secrets. The loud element,
 * the warning [`IntegrationsBanner`], appears only when a key genuinely needs a human;
 * the rest of the time this reads as a quiet inventory.
 */
export function IntegrationsPage() {
  const { data: credentials, isPending, isError, error } = useCredentials()

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div className={styles.headingRow}>
          <span className={styles.headingIcon} aria-hidden="true">
            <PlugZap size={24} strokeWidth={2} />
          </span>
          <h1>Integrations</h1>
        </div>
        <p className={styles.lede}>
          API keys for the services Atlas connects to. Keys are encrypted at rest and never
          shown again after you add them — Atlas keeps only the last four characters, to help
          you tell them apart.
        </p>
      </header>

      {/* The requested expiry/re-auth warning. Renders nothing unless a key needs attention. */}
      <IntegrationsBanner />

      {isPending ? (
        <div className={styles.list}>
          {PROVIDERS.map((provider) => (
            <Skeleton key={provider} height="120px" className={styles.skeleton} />
          ))}
        </div>
      ) : isError ? (
        <Banner appearance="error">
          {error instanceof ApiError && error.status === 403
            ? 'Integration keys are managed by instance administrators. Ask an admin to add or update them.'
            : error instanceof ApiError
              ? (error.problem?.detail ?? 'Could not load the integration keys.')
              : 'Could not load the integration keys.'}
        </Banner>
      ) : (
        <div className={styles.list}>
          {(() => {
            const grouped = groupByProvider(credentials)
            return PROVIDERS.map((provider) => (
              <ProviderSection
                key={provider}
                provider={provider}
                credentials={grouped[provider]}
              />
            ))
          })()}
        </div>
      )}
    </div>
  )
}
