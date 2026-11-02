import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

export type Notification = components['schemas']['Notification']
export type Preferences = components['schemas']['Preferences']
export type Inbox = components['schemas']['Inbox']

export async function fetchInbox(): Promise<Inbox> {
  return unwrap(await api.GET('/api/v1/notifications', {}))
}

export async function markRead(id: string): Promise<void> {
  unwrap(await api.POST('/api/v1/notifications/{id}/read', { params: { path: { id } } }))
}

export async function markAllRead(): Promise<void> {
  unwrap(await api.POST('/api/v1/notifications/read-all', {}))
}

export async function fetchPreferences(): Promise<Preferences> {
  return unwrap(await api.GET('/api/v1/notifications/preferences', {}))
}

export async function savePreferences(prefs: Preferences): Promise<Preferences> {
  return unwrap(await api.PUT('/api/v1/notifications/preferences', { body: prefs }))
}
