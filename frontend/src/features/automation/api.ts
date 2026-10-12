import { ApiError, isProblem } from '@/lib/api'
import type { AgentProvider } from '@/features/card-detail/api'

/** Mirrors the backend automation DTOs. */
export interface AutoRun {
  id: string; projectId: string; provider: string; status: string; maxTasks: number;
  completedTasks: number; currentCardKey: string | null; currentSessionId: string | null;
  message: string; console: string; startedAt: string; updatedAt: string;
}
export interface CardAutomation {
  assignment: { provider: AgentProvider; reason: string } | null;
  blockedBy: { key: string; summary: string; reason: string; done: boolean }[];
}
async function request<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const response = await fetch(new Request(new URL(`/api/v1${path}`, window.location.origin), {
    method, credentials: 'include', headers: { 'Content-Type': 'application/json' },
    ...(body !== undefined && { body: JSON.stringify(body) }),
  }))
  if (!response.ok) {
    const problem: unknown = await response.json().catch(() => null)
    throw new ApiError(isProblem(problem) ? problem.detail : 'Could not update automation.', response.status, isProblem(problem) ? problem : undefined)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}
export const fetchAuto = (key: string) => request<AutoRun | null>(`/projects/${encodeURIComponent(key)}/auto`)
export const startAuto = (key: string, provider: AgentProvider, maxTasks: number) => request<AutoRun>(`/projects/${encodeURIComponent(key)}/auto`, 'POST', { provider, maxTasks })
export const stopAuto = (key: string) => request<void>(`/projects/${encodeURIComponent(key)}/auto/stop`, 'POST')
export const fetchCardAutomation = (key: string) => request<CardAutomation>(`/cards/${encodeURIComponent(key)}/automation`)
export const addDependency = (key: string, blockedBy: string, reason: string) => request<CardAutomation>(`/cards/${encodeURIComponent(key)}/dependencies`, 'POST', { blockedBy, reason })
export const removeDependency = (key: string, blockerKey: string) => request<void>(`/cards/${encodeURIComponent(key)}/dependencies/${encodeURIComponent(blockerKey)}`, 'DELETE')

export interface AgentActivity {
  id: string; provider: string; message: string; cardKey: string | null;
  sessionId: string | null; createdAt: string;
}
export interface ProviderUsage {
  provider: string; runs: number; activeRuns: number; measuredRuns: number; inputTokens: number | null;
  outputTokens: number | null; cachedTokens: number | null; costUsd: number | null;
}
export const fetchAutoActivity = (key: string) => request<AgentActivity[]>(`/projects/${encodeURIComponent(key)}/auto/activity`)
export const fetchProviderUsage = () => request<ProviderUsage[]>('/agent-usage')
