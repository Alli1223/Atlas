/** One line of a live agent session's output, reduced to what is worth showing. */
export interface LiveOutputLine {
  /** A stable key — the line's index is not enough once lines can arrive twice at the
   * boundary of connecting (see `agent::orchestrator::subscribe`'s doc), so callers should
   * key on the rendered array index only as a last resort. */
  kind: 'system' | 'assistant' | 'tool-use' | 'tool-result' | 'result' | 'other'
  text: string
}

/** `value` if it is a non-empty string, otherwise `fallback` — never `String(unknown)`,
 * which would stringify an object as the useless `[object Object]`. */
function stringOr(value: unknown, fallback: string): string {
  return typeof value === 'string' && value !== '' ? value : fallback
}

/**
 * Reduces one raw `stream-json` line to a short, human-readable summary — the same
 * best-effort spirit as the backend's own `agent::claude_code` parser, which tags anything
 * outside its known shapes as `Unknown` rather than failing: a line this cannot make sense
 * of is shown as itself, verbatim, never dropped.
 */
export function summarizeLine(raw: string): LiveOutputLine {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return { kind: 'other', text: raw }
  }
  if (typeof parsed !== 'object' || parsed === null) {
    return { kind: 'other', text: raw }
  }
  const event = parsed as Record<string, unknown>

  switch (event.type) {
    case 'system':
      return {
        kind: 'system',
        text: `Session started (${stringOr(event.model, 'unknown model')})`,
      }
    case 'result':
      return { kind: 'result', text: stringOr(event.result, 'Finished') }
    case 'assistant':
    case 'user':
      return summarizeMessage(event)
    default:
      return { kind: 'other', text: raw }
  }
}

/** Content blocks, the same shape the Anthropic Messages API (and so the CLI's stream-json
 * passthrough) uses: `{ type: 'text', text }`, `{ type: 'tool_use', name, input }`,
 * `{ type: 'tool_result', ... }`. */
function summarizeMessage(event: Record<string, unknown>): LiveOutputLine {
  const message = event.message as Record<string, unknown> | undefined
  const content = message?.content
  const blocks = Array.isArray(content) ? content : []

  for (const block of blocks) {
    if (typeof block !== 'object' || block === null) continue
    const b = block as Record<string, unknown>
    if (b.type === 'text' && typeof b.text === 'string' && b.text.trim() !== '') {
      return { kind: 'assistant', text: b.text }
    }
    if (b.type === 'tool_use') {
      return { kind: 'tool-use', text: `Using ${stringOr(b.name, 'a tool')}` }
    }
    if (b.type === 'tool_result') {
      return { kind: 'tool-result', text: 'Tool result received' }
    }
  }

  // A recognised message with no block this function knows how to summarise (e.g. an
  // image, or a future block type) — the event's own JSON is still shown, just unreduced.
  return { kind: 'other', text: JSON.stringify(event) }
}
