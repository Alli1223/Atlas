import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** A project, as the API describes it. Mirrors `crate::domain::project::ProjectDto`. */
export type Project = components['schemas']['ProjectDto']

/** A project template descriptor. Mirrors `crate::api::projects::TemplateDto`. */
export type ProjectTemplate = components['schemas']['TemplateDto']

/** The template identifier a new project is seeded from. */
export type Template = components['schemas']['Template']

export interface CreateProjectInput {
  key: string
  name: string
  description?: string
  template?: Template
}

/**
 * Every project the caller can see.
 *
 * The backend already filters this to the projects the signed-in user has access to —
 * an inaccessible project is absent, not forbidden — so the grid renders exactly what
 * the server returns without a second access check here.
 */
export async function fetchProjects(): Promise<Project[]> {
  return unwrap(await api.GET('/api/v1/projects'))
}

/** One project by key, or throws `ApiError` (404 when it does not exist or is hidden). */
export async function fetchProject(key: string): Promise<Project> {
  return unwrap(await api.GET('/api/v1/projects/{key}', { params: { path: { key } } }))
}

/** The templates a new project may be seeded from. */
export async function fetchTemplates(): Promise<ProjectTemplate[]> {
  return unwrap(await api.GET('/api/v1/project-templates'))
}

/**
 * Creates a project. The key is uppercased server-side and fixes the card-key prefix
 * forever, so it cannot be renamed later — the create form is the one place it is set.
 */
export async function createProject(input: CreateProjectInput): Promise<Project> {
  return unwrap(
    await api.POST('/api/v1/projects', {
      body: {
        key: input.key,
        name: input.name,
        ...(input.description !== undefined && { description: input.description }),
        ...(input.template !== undefined && { template: input.template }),
      },
    }),
  )
}
