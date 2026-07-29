import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/**
 * A stored credential, as the API describes it — **metadata only, never the secret.**
 *
 * Mirrors `crate::secrets::CredentialDto`. There is deliberately no field for the
 * plaintext, the ciphertext, or the nonce: the backend's DTO cannot carry them, so
 * neither can this type. `lastFour` is all the UI ever sees of the key itself.
 */
export type Credential = components['schemas']['CredentialDto']

/**
 * Which integration a credential is for. A closed set, pinned server-side by a `CHECK`
 * and an enum. Mirrors `crate::secrets::Provider`.
 */
export type Provider = components['schemas']['Provider']

/**
 * The effective status pill, resolved server-side against the current clock.
 *
 * Mirrors `crate::secrets::PillStatus`. `expiring` and `expired` are *derived* from
 * `expiresAt` and now — the backend computes them so the client never has to guess, and
 * so an expiry that lapses between page loads reads correctly without a re-probe.
 */
export type PillStatus = components['schemas']['PillStatus']

/** The body of `POST /credentials`. Mirrors `crate::api::credentials::CreateCredentialRequest`. */
export interface CreateCredentialInput {
  provider: Provider
  label: string
  /** The secret itself. Sent once, over the wire, and never returned. */
  secret: string
}

/**
 * Every stored credential, as metadata.
 *
 * Admin-only server-side (the routes name `RequireAdmin`), so a non-admin gets a 403 and
 * this rejects — the caller decides whether to even show the screen.
 */
export async function fetchCredentials(): Promise<Credential[]> {
  return unwrap(await api.GET('/api/v1/credentials'))
}

/**
 * Stores a new credential, encrypting the secret at rest.
 *
 * The response is metadata only: the secret the caller just sent is gone from the client
 * the moment this resolves — there is nothing in the returned value to echo it back.
 */
export async function createCredential(input: CreateCredentialInput): Promise<Credential> {
  return unwrap(
    await api.POST('/api/v1/credentials', {
      body: {
        provider: input.provider,
        label: input.label,
        secret: input.secret,
      },
    }),
  )
}

/** Deletes a credential. 404 for one that does not exist. */
export async function deleteCredential(id: string): Promise<void> {
  unwrap(await api.DELETE('/api/v1/credentials/{id}', { params: { path: { id } } }))
}

/**
 * Validates a credential against its provider, on demand, and returns the updated
 * metadata — status, discovered scopes and expiry, and a fresh `lastValidatedAt`.
 */
export async function validateCredential(id: string): Promise<Credential> {
  return unwrap(
    await api.POST('/api/v1/credentials/{id}/validate', { params: { path: { id } } }),
  )
}
