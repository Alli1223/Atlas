import { useEffect, useRef } from 'react'

import styles from './LiveOutput.module.css'
import { summarizeLine } from './liveOutputLine'
import { useLiveAgentSessionOutput } from './useLiveAgentSessionOutput'

/**
 * A running session's output, tailing live over WebSocket — see
 * `useLiveAgentSessionOutput`. Each line is reduced to a short summary (`summarizeLine`)
 * rather than rendered as raw JSON; a line this cannot make sense of still shows up, just
 * unreduced, the same "never drop it" spirit the backend's own event parser keeps.
 *
 * Rendered only for the session actually `running` — a finished one has nothing live left
 * to show, and reads back through the ordinary transcript endpoint instead (not yet wired
 * to a UI of its own; this component's scope is the live case `TODO.md` names).
 */
export function LiveOutput({ sessionId }: { sessionId: string }) {
  const lines = useLiveAgentSessionOutput(sessionId, true)
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    // jsdom has no layout engine and does not implement `scrollIntoView` at all — guarded
    // rather than assumed, so a test rendering this needs no polyfill to avoid a crash.
    bottomRef.current?.scrollIntoView?.({ block: 'end' })
  }, [lines.length])

  return (
    <div className={styles.output} role="log" aria-label="Live output">
      {lines.length === 0 ? (
        <p className={styles.waiting}>Waiting for output…</p>
      ) : (
        lines.map((line, index) => {
          const { kind, text } = summarizeLine(line)
          return (
            <p key={index} className={styles.line} data-kind={kind}>
              {text}
            </p>
          )
        })
      )}
      <div ref={bottomRef} />
    </div>
  )
}
