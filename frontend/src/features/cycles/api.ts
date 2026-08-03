import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** A cycle. Mirrors `crate::domain::cycle::Cycle`. */
export type Cycle = components['schemas']['Cycle']
/** A cycle's lifecycle state. */
export type CycleState = components['schemas']['CycleState']

export interface CreateCycleInput {
  projectKey: string
  name: string
  goal?: string
}

export interface UpdateCycleInput {
  id: string
  name?: string
  /** `null` clears the goal, a string sets it, absent leaves it. */
  goal?: string | null
}

export interface StartCycleInput {
  id: string
  startDate: string
  endDate: string
}

export interface ReopenCycleInput {
  id: string
  /** The replanned end date. The start date is kept as it was. */
  endDate: string
}

/** Where a completing cycle's incomplete cards go. */
export type CarryTo =
  | { kind: 'backlog' }
  | { kind: 'existingCycle'; cycleId: string }
  | { kind: 'newCycle'; name: string }

export interface CompleteCycleInput {
  id: string
  carryTo: CarryTo
}

/** A project's cycles: active first, then future, then closed. */
export async function fetchProjectCycles(projectKey: string): Promise<Cycle[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/cycles', { params: { path: { key: projectKey } } }),
  )
}

/** Creates a cycle. The project must have cycles enabled. */
export async function createCycle({ projectKey, name, goal }: CreateCycleInput): Promise<Cycle> {
  return unwrap(
    await api.POST('/api/v1/projects/{key}/cycles', {
      params: { path: { key: projectKey } },
      body: { name, ...(goal !== undefined && { goal }) },
    }),
  )
}

/** Renames a cycle and/or edits its goal. Legal in any state. */
export async function updateCycle({ id, name, goal }: UpdateCycleInput): Promise<Cycle> {
  return unwrap(
    await api.PATCH('/api/v1/cycles/{id}', {
      params: { path: { id } },
      body: {
        ...(name !== undefined && { name }),
        // `null` clears the goal and `undefined` leaves it, so this cannot collapse into `??`.
        ...(goal !== undefined && { goal }),
      },
    }),
  )
}

/** Starts a cycle: `future -> active`. */
export async function startCycle({ id, startDate, endDate }: StartCycleInput): Promise<Cycle> {
  return unwrap(
    await api.POST('/api/v1/cycles/{id}/start', {
      params: { path: { id } },
      body: { startDate, endDate },
    }),
  )
}

/** Completes a cycle: `active -> closed`, carrying any incomplete cards as directed. */
export async function completeCycle({ id, carryTo }: CompleteCycleInput): Promise<Cycle> {
  return unwrap(
    await api.POST('/api/v1/cycles/{id}/complete', {
      params: { path: { id } },
      body: { carryTo },
    }),
  )
}

/** Reopens a closed cycle: `closed -> active`, replanning its end date. */
export async function reopenCycle({ id, endDate }: ReopenCycleInput): Promise<Cycle> {
  return unwrap(
    await api.POST('/api/v1/cycles/{id}/reopen', {
      params: { path: { id } },
      body: { endDate },
    }),
  )
}

/**
 * The cycle a card currently belongs to, or `null`.
 *
 * "Not in a cycle" is the common, expected state, so the endpoint's 404 is folded to `null`
 * rather than thrown.
 */
export async function fetchCardCycle(cardKey: string): Promise<Cycle | null> {
  const result = await api.GET('/api/v1/cards/{key}/cycle', { params: { path: { key: cardKey } } })
  if (result.response.status === 404) return null
  return unwrap(result)
}

/** Adds a card to a cycle (or refreshes its membership, if previously removed). */
export async function addCardToCycle(cardKey: string, cycleId: string): Promise<void> {
  unwrap(
    await api.POST('/api/v1/cards/{key}/cycle', {
      params: { path: { key: cardKey } },
      body: { cycleId },
    }),
  )
}

/** Removes a card from its current cycle. A no-op if it was not in one. */
export async function removeCardFromCycle(cardKey: string): Promise<void> {
  unwrap(await api.DELETE('/api/v1/cards/{key}/cycle', { params: { path: { key: cardKey } } }))
}
