/** System telemetry snapshot. Mirrors `crate::api::admin::SystemStats`. */
export interface SystemStats {
  cpuUsagePercent: number
  memoryTotalBytes: number
  memoryUsedBytes: number
  diskTotalBytes: number
  diskUsedBytes: number
}

/** GitHub release poll result. Mirrors `crate::api::admin::UpdateStatus`. */
export interface UpdateStatus {
  currentVersion: string
  latestVersion: string | null
  hasUpdate: boolean
  releaseUrl: string | null
  releaseNotes: string | null
  error: string | null
}

/** Queued-update confirmation. Mirrors `crate::api::admin::ApplyUpdateResponse`. */
export interface ApplyUpdateResponse {
  message: string
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: 'same-origin' })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json() as Promise<T>
}

async function post<T>(path: string): Promise<T> {
  const res = await fetch(path, { method: 'POST', credentials: 'same-origin' })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json() as Promise<T>
}

export function fetchSystemStats(): Promise<SystemStats> {
  return get<SystemStats>('/api/v1/admin/system')
}

export function fetchUpdateStatus(): Promise<UpdateStatus> {
  return get<UpdateStatus>('/api/v1/admin/updates')
}

export function applyUpdate(): Promise<ApplyUpdateResponse> {
  return post<ApplyUpdateResponse>('/api/v1/admin/updates/apply')
}
