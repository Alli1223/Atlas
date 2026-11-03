import { describe, expect, it } from 'vitest'
import { consoleEntry } from './agent-console'

describe('agent console', () => {
  it('shows commands and their output', () => {
    expect(consoleEntry(JSON.stringify({type: 'item.completed', item: {type: 'command_execution', command: 'cargo test', aggregated_output: 'Tests passed', exit_code: 0, status: 'completed'}})).text).toContain('$ cargo test\nTests passed')
  })
  it('highlights explicit questions and denied permissions', () => {
    expect(consoleEntry('{"type":"input_required","question":"Which database?"}')).toEqual({text: 'Which database?', attention: true})
    expect(consoleEntry('{"type":"result","permission_denials":[{"tool_name":"Bash"}]}').attention).toBe(true)
    expect(consoleEntry('{"event":"step_update","step_update":{"tool_name":"ask_question","tool_info":{"parameters":{"question":"Which database?"}}}}').attention).toBe(true)
  })
  it('keeps unknown and plain diagnostics visible without guessing they are questions', () => {
    expect(consoleEntry('Compile failed?')).toEqual({text: 'Compile failed?', attention: false})
    expect(consoleEntry('{"type":"new_event"}').text).toContain('new_event')
  })
  it('renders Claude text and tools without private reasoning', () => {
    const output = consoleEntry('{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"Reading files"},{"type":"tool_use","name":"Read","input":{"file_path":"README.md"}}]}}')
    expect(output.text).toContain('Reading files\nTool: Read')
    expect(output.text).not.toContain('private')
  })
})
