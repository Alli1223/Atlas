/**
 * PLACEHOLDER — replace by running `npm run gen:api` against a running backend:
 *
 *   openapi-typescript http://127.0.0.1:8080/api/openapi.json -o src/lib/api-schema.d.ts
 *
 * The backend emits this document via utoipa. Commit the regenerated file, and have CI
 * regenerate and fail on diff — that turns "a Rust DTO changed and the frontend silently
 * broke" into a red build, which is the whole point of generating the types.
 *
 * This stub mirrors openapi-typescript@7's real output shape so `src/lib/api.ts` type-checks
 * before the backend exists. The single `/projects` operation is illustrative only.
 *
 * Watch when regenerating: Atlas's recursive Card -> Board -> Card model is exactly where
 * $ref resolution gets weird (infinite expansion, or `unknown`). Check it early.
 */

export interface paths {
  '/projects': {
    parameters: {
      query?: never
      header?: never
      path?: never
      cookie?: never
    }
    get: operations['listProjects']
    put?: never
    post?: never
    delete?: never
    options?: never
    head?: never
    patch?: never
    trace?: never
  }
}

export type webhooks = Record<string, never>

export interface components {
  schemas: {
    Project: {
      id: string
      /** Project key, e.g. "ATLAS". */
      key: string
      name: string
    }
  }
  responses: never
  parameters: never
  requestBodies: never
  headers: never
  pathItems: never
}

export type $defs = Record<string, never>

export interface operations {
  listProjects: {
    parameters: {
      query?: never
      header?: never
      path?: never
      cookie?: never
    }
    requestBody?: never
    responses: {
      200: {
        headers: {
          [name: string]: unknown
        }
        content: {
          'application/json': components['schemas']['Project'][]
        }
      }
    }
  }
}
