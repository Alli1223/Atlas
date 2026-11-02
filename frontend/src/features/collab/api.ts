import { ApiError, api, isProblem, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

export type Watcher = components['schemas']['Watcher']
export type WorklogSummary = components['schemas']['WorklogSummary']
export type CardLink = components['schemas']['CardLink']
export type RemoteLink = components['schemas']['RemoteLink']
export type Attachment = components['schemas']['Attachment']
export type CardFieldValue = components['schemas']['CardFieldValue']
export type LinkType = components['schemas']['LinkType']

const card = (key: string) => ({ params: { path: { key } } })

export const listWatchers = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/watchers', card(key)))
export const watch = async (key: string) =>
  unwrap(await api.POST('/api/v1/cards/{key}/watchers', card(key)))
export const unwatch = async (key: string, userId: string) =>
  unwrap(
    await api.DELETE('/api/v1/cards/{key}/watchers/{userId}', {
      params: { path: { key, userId } },
    }),
  )

export const listWorklogs = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/worklogs', card(key)))
export const logWork = async (key: string, duration: string, note: string) =>
  unwrap(
    await api.POST('/api/v1/cards/{key}/worklogs', {
      ...card(key),
      body: { duration, note: note.trim() || null },
    }),
  )
export const deleteWorklog = async (id: string) =>
  unwrap(await api.DELETE('/api/v1/worklogs/{id}', { params: { path: { id } } }))

export const listLinks = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/links', card(key)))
export const linkCard = async (key: string, targetKey: string, linkType: LinkType) =>
  unwrap(
    await api.POST('/api/v1/cards/{key}/links', {
      ...card(key),
      body: { targetKey: targetKey.trim().toUpperCase(), linkType },
    }),
  )
export const unlinkCard = async (key: string, linkId: string) =>
  unwrap(
    await api.DELETE('/api/v1/cards/{key}/links/{linkId}', { params: { path: { key, linkId } } }),
  )

export const listRemoteLinks = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/remote-links', card(key)))
export const addRemoteLink = async (key: string, url: string, title: string) =>
  unwrap(
    await api.POST('/api/v1/cards/{key}/remote-links', {
      ...card(key),
      body: { url: url.trim(), title: title.trim() || null },
    }),
  )
export const removeRemoteLink = async (key: string, linkId: string) =>
  unwrap(
    await api.DELETE('/api/v1/cards/{key}/remote-links/{linkId}', {
      params: { path: { key, linkId } },
    }),
  )

export const listAttachments = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/attachments', card(key)))
export const deleteAttachment = async (id: string) =>
  unwrap(await api.DELETE('/api/v1/attachments/{id}', { params: { path: { id } } }))

/** Streams a file up as the request body — the endpoint takes raw bytes, not multipart. */
async function sendFile(url: string, file: File): Promise<Attachment> {
  const response = await fetch(url, {
    method: 'POST',
    body: file,
    credentials: 'include',
    headers: { 'Content-Type': file.type || 'application/octet-stream' },
  })
  const body: unknown = await response.json().catch(() => undefined)
  if (!response.ok) {
    const problem = isProblem(body) ? body : undefined
    throw new ApiError(problem?.detail ?? `Upload failed (${response.status})`, response.status, problem)
  }
  return body as Attachment
}

export const uploadAttachment = (key: string, file: File) =>
  sendFile(
    `/api/v1/cards/${encodeURIComponent(key)}/attachments?filename=${encodeURIComponent(file.name)}`,
    file,
  )
export const uploadVersion = (id: string, file: File) =>
  sendFile(
    `/api/v1/attachments/${encodeURIComponent(id)}/versions?filename=${encodeURIComponent(file.name)}`,
    file,
  )
export const attachmentUrl = (id: string) => `/api/v1/attachments/${encodeURIComponent(id)}/content`

export const listFields = async (key: string) =>
  unwrap(await api.GET('/api/v1/cards/{key}/fields', card(key)))
export const setField = async (key: string, fieldId: string, value: unknown) =>
  unwrap(
    await api.PATCH('/api/v1/cards/{key}/fields', {
      ...card(key),
      body: { values: { [fieldId]: value } } as never,
    }),
  )
