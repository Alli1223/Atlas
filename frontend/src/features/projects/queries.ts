import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import type { ApiError } from '@/lib/api'

import * as projectsApi from './api'
import type { CreateProjectInput, Project } from './api'

/**
 * Query keys for everything projects.
 *
 * A single object rather than scattered string literals: `invalidateQueries` and
 * `useQuery` must agree exactly, and a typo in one of them fails *silently*. Mirrors
 * `authKeys` and `tagKeys`.
 */
export const projectKeys = {
  all: ['projects'] as const,
  list: () => [...projectKeys.all, 'list'] as const,
  detail: (key: string) => [...projectKeys.all, 'detail', key] as const,
  templates: () => [...projectKeys.all, 'templates'] as const,
}

/** Every project the caller can see. */
export function projectsQueryOptions() {
  return queryOptions({
    queryKey: projectKeys.list(),
    queryFn: projectsApi.fetchProjects,
  })
}

/** One project by key. */
export function projectQueryOptions(key: string) {
  return queryOptions({
    queryKey: projectKeys.detail(key),
    queryFn: () => projectsApi.fetchProject(key),
  })
}

/** Every project the caller can see. */
export function useProjects() {
  return useQuery(projectsQueryOptions())
}

/** One project by key. */
export function useProject(key: string) {
  return useQuery(projectQueryOptions(key))
}

/** The templates a new project may be seeded from. */
export function useTemplates() {
  return useQuery({
    queryKey: projectKeys.templates(),
    queryFn: projectsApi.fetchTemplates,
    // Templates are a static, seeded list — they never change within a session.
    staleTime: Infinity,
  })
}

/**
 * Creates a project and refreshes the list.
 *
 * Invalidates rather than pushing the new project into the cached list: the server
 * orders the list and derives access, and a client that guessed either would be wrong.
 */
export function useCreateProject() {
  const queryClient = useQueryClient()

  return useMutation<Project, ApiError, CreateProjectInput>({
    mutationFn: projectsApi.createProject,
    onSuccess: (project) => {
      queryClient.setQueryData(projectKeys.detail(project.key), project)
      void queryClient.invalidateQueries({ queryKey: projectKeys.list() })
    },
  })
}
