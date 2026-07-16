/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_URL?: string
  readonly VITE_WS_URL?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}

// NOTE: no VITE_-prefixed variable may ever hold a secret. Anything with the VITE_ prefix
// is inlined into the client bundle in plaintext, which is a direct violation of the
// "secrets never appear in API responses/logs" rule. PATs and API keys live encrypted in
// the backend vault and are never sent to the client.
