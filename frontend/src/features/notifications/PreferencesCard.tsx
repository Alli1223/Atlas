import { Checkbox, Select, Spinner } from '@/components/ui'
import kit from '@/styles/kit.module.css'

import type { Preferences } from './api'
import { usePreferences, useSavePreferences } from './queries'

const CHANNELS = [
  { value: 'in_app', label: 'In app' },
  { value: 'email', label: 'In app + email' },
  { value: 'off', label: 'Off' },
]

/** Per-kind delivery, "notify me of my own changes", and the daily digest (`TODO.md` Phase 17). */
export function PreferencesCard() {
  const prefs = usePreferences()
  const save = useSavePreferences()
  if (prefs.isPending) return <Spinner />
  if (!prefs.data) return null
  const current = prefs.data
  const update = (next: Partial<Preferences>) => save.mutate({ ...current, ...next })
  return (
    <section className={kit.section} aria-label="Notification preferences">
      <h2 className={kit.subtitle}>Notifications</h2>
      <table className={kit.table}>
        <tbody>
          {current.channels.map((entry) => (
            <tr key={entry.kind}>
              <td>{entry.kind.replace(/_/g, ' ')}</td>
              <td>
                <Select
                  aria-label={`Delivery for ${entry.kind}`}
                  options={CHANNELS}
                  value={entry.channel}
                  onChange={(e) =>
                    update({
                      channels: current.channels.map((c) =>
                        c.kind === entry.kind ? { ...c, channel: e.target.value as typeof c.channel } : c,
                      ),
                    })
                  }
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <Checkbox
        label="Notify me of my own changes"
        checked={current.notifyOwnChanges}
        onChange={(e) => update({ notifyOwnChanges: e.target.checked })}
      />
      <Checkbox
        label="Send me a daily digest email"
        checked={current.dailyDigest}
        onChange={(e) => update({ dailyDigest: e.target.checked })}
      />
    </section>
  )
}
