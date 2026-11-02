import { useSyncExternalStore } from 'react'

import kit from '@/styles/kit.module.css'

function subscribe(onChange: () => void) {
  window.addEventListener('online', onChange)
  window.addEventListener('offline', onChange)
  return () => {
    window.removeEventListener('online', onChange)
    window.removeEventListener('offline', onChange)
  }
}

/** A strip at the foot of the window while the browser reports no network. */
export function OfflineBanner() {
  const online = useSyncExternalStore(
    subscribe,
    () => navigator.onLine,
    () => true,
  )
  if (online) return null
  return (
    <div className={kit.offline} role="status">
      You are offline. Changes will fail until the connection is back.
    </div>
  )
}
