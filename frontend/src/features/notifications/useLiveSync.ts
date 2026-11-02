import { useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'

import { notificationKeys } from './queries'

/** A frame on the live channel. Only the fields this hook reads are typed. */
interface Frame {
  type?: string
  cardKey?: string
  projectKey?: string
}

/** Where the live channel is: same origin, `ws:`/`wss:` to match the page. */
export function liveUrl(location: Pick<Location, 'protocol' | 'host'> = window.location): string {
  return `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/v1/ws`
}

/**
 * Keeps the query cache in step with the server over `/api/v1/ws` (`TODO.md` Phase 8: "merge
 * pushed events into the Query cache").
 *
 * A frame is a cache-invalidation hint, never data to trust: a card event invalidates the
 * boards and that card's detail, a notification refreshes the bell, and `resync` (the
 * server's "you fell behind") invalidates everything. Reconnects with backoff, capped at
 * 30 s, so a restarting server is picked up without a reload.
 */
export function useLiveSync(enabled: boolean): void {
  const queryClient = useQueryClient()

  useEffect(() => {
    if (!enabled || typeof WebSocket === 'undefined') return
    let socket: WebSocket | undefined
    let retry: ReturnType<typeof setTimeout> | undefined
    let attempt = 0
    let closed = false

    const connect = () => {
      socket = new WebSocket(liveUrl())
      socket.onopen = () => {
        attempt = 0
      }
      socket.onmessage = (event: MessageEvent<string>) => {
        let frame: Frame
        try {
          frame = JSON.parse(event.data) as Frame
        } catch {
          return
        }
        switch (frame.type) {
          case 'notification':
            void queryClient.invalidateQueries({ queryKey: notificationKeys.all })
            break
          case 'resync':
            void queryClient.invalidateQueries()
            break
          case 'card.created':
          case 'card.updated':
          case 'comment.added':
            void queryClient.invalidateQueries({ queryKey: ['board'] })
            void queryClient.invalidateQueries({ queryKey: ['search'] })
            if (frame.cardKey) {
              const cardKey = frame.cardKey
              void queryClient.invalidateQueries({
                predicate: (query) => query.queryKey.includes(cardKey),
              })
            }
            break
          default:
            break
        }
      }
      socket.onclose = () => {
        if (closed) return
        attempt += 1
        retry = setTimeout(connect, Math.min(30_000, 1000 * 2 ** attempt))
      }
    }

    connect()
    return () => {
      closed = true
      if (retry) clearTimeout(retry)
      socket?.close()
    }
  }, [enabled, queryClient])
}
