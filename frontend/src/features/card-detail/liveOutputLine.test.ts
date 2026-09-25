import { describe, expect, it } from 'vitest'

import { summarizeLine } from './liveOutputLine'

describe('summarizeLine', () => {
  it('shows unparseable JSON verbatim rather than dropping it', () => {
    expect(summarizeLine('not json at all')).toEqual({ kind: 'other', text: 'not json at all' })
  })

  it('summarises a system/init line', () => {
    const line = JSON.stringify({ type: 'system', subtype: 'init', model: 'claude-opus-5' })
    expect(summarizeLine(line)).toEqual({
      kind: 'system',
      text: 'Session started (claude-opus-5)',
    })
  })

  it('summarises the terminal result line', () => {
    const line = JSON.stringify({ type: 'result', subtype: 'success', result: 'All done' })
    expect(summarizeLine(line)).toEqual({ kind: 'result', text: 'All done' })
  })

  it('extracts assistant text from the message content blocks', () => {
    const line = JSON.stringify({
      type: 'assistant',
      message: { content: [{ type: 'text', text: 'Reading the file now.' }] },
    })
    expect(summarizeLine(line)).toEqual({ kind: 'assistant', text: 'Reading the file now.' })
  })

  it('summarises a tool_use block as "Using <tool>"', () => {
    const line = JSON.stringify({
      type: 'assistant',
      message: { content: [{ type: 'tool_use', name: 'Bash', input: { command: 'ls' } }] },
    })
    expect(summarizeLine(line)).toEqual({ kind: 'tool-use', text: 'Using Bash' })
  })

  it('summarises a tool_result block on a user event', () => {
    const line = JSON.stringify({
      type: 'user',
      message: { content: [{ type: 'tool_result', content: 'file1.txt\nfile2.txt' }] },
    })
    expect(summarizeLine(line)).toEqual({ kind: 'tool-result', text: 'Tool result received' })
  })

  it('falls back to the raw event JSON when no block is recognised', () => {
    const event = { type: 'assistant', message: { content: [{ type: 'image' }] } }
    expect(summarizeLine(JSON.stringify(event))).toEqual({
      kind: 'other',
      text: JSON.stringify(event),
    })
  })

  it('treats an unrecognised top-level type as other, verbatim', () => {
    const line = JSON.stringify({ type: 'rate_limit_event', limit: 100 })
    expect(summarizeLine(line)).toEqual({ kind: 'other', text: line })
  })
})
