import { useEffect, useRef, useState } from 'react'

/**
 * Subscribes to a running agent session's live `stream-json` output over
 * `GET /api/v1/agent-sessions/{id}/live` — see `agent::orchestrator`'s "Streaming a run live"
 * doc for the wire format (one raw line per WebSocket text message) and why a client
 * connecting mid-run still sees every line from the start.
 *
 * A plain `WebSocket`, not `openapi-fetch` — there is no REST shape for this at all, and the
 * session cookie already rides along on the upgrade request the same way it does on every
 * other same-origin call, so there is nothing extra to wire up for auth.
 *
 * Not a `useQuery`: this is a live subscription with its own connection lifecycle, not
 * something with a cacheable "current value" TanStack Query's model fits — the accumulated
 * `lines` *is* the state, and there is nothing to refetch.
 */
export function useLiveAgentSessionOutput(sessionId: string, enabled: boolean) {
  const [lines, setLines] = useState<string[]>([])
  const closedRef = useRef(false)

  useEffect(() => {
    if (!enabled) return

    closedRef.current = false

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const socket = new WebSocket(
      `${protocol}//${window.location.host}/api/v1/agent-sessions/${sessionId}/live`,
    )

    // Cleared here, in response to the connection actually opening, rather than
    // synchronously at the top of the effect — the same "setState reacts to an external
    // event" shape the message handler below already has.
    socket.onopen = () => setLines([])
    socket.onmessage = (event: MessageEvent<string>) => {
      setLines((previous) => [...previous, event.data])
    }
    socket.onclose = () => {
      closedRef.current = true
    }

    return () => {
      // A socket the run has already finished (and the server has therefore already
      // closed) throws on a second close — harmless, but silenced rather than left as
      // console noise on every ordinary unmount.
      if (!closedRef.current) socket.close()
    }
  }, [sessionId, enabled])

  return lines
}
