type ObjectValue = Record<string, unknown>
const object = (value: unknown): ObjectValue => value && typeof value === 'object' && !Array.isArray(value) ? value as ObjectValue : {}
const text = (value: unknown): string => typeof value === 'string' ? value : ''
const printable = (value: unknown): string => typeof value === 'string' ? value : JSON.stringify(value ?? '')

export interface ConsoleEntry { text: string; attention: boolean }

/** Translate known provider events, retaining unknown output for diagnosis. */
export function consoleEntry(line: string): ConsoleEntry {
  let event: ObjectValue
  try { event = object(JSON.parse(line)) } catch { return { text: line, attention: false } }
  const kind = text(event.type) || text(event.event)
  const step = object(event.step_update)
  const tool = text(step.tool_name)
  const content = object(event.message).content
  const blocks = Array.isArray(content) ? content.map(object) : []
  const questions = blocks.filter(block => block.name === 'AskUserQuestion')
  const attention = ['input_required', 'user_input_required', 'permission_request'].includes(kind)
    || questions.length > 0 || ['ask_question', 'ask_permission', 'ask_custom_permission'].includes(tool)
    || (Array.isArray(event.permission_denials) && event.permission_denials.length > 0)
  if (attention) return { text: text(event.question) || text(event.message) || questions.map(block => printable(block.input)).join('\n') || printable(step.tool_info ?? event.permission_denials ?? event), attention }
  if (kind === 'atlas_console') return { text: `[${text(event.stream)}] ${text(event.text)}`, attention }
  if (blocks.length) return { text: blocks.map(block => {
    if (block.type === 'text') return text(block.text)
    if (block.type === 'tool_use') return `Tool: ${text(block.name)}\n${printable(block.input)}`
    if (block.type === 'tool_result') return `Tool output:\n${printable(block.content)}`
    return '' // Do not display private reasoning blocks.
  }).filter(Boolean).join('\n'), attention }
  const item = object(event.item)
  if (item.type === 'agent_message') return { text: text(item.text), attention }
  if (item.type === 'command_execution') return { text: `$ ${text(item.command)}\n${text(item.aggregated_output)}\n${text(item.status)}${typeof item.exit_code === 'number' ? ` (exit ${item.exit_code})` : ''}`, attention }
  if (item.type === 'file_change') return { text: `Files ${text(item.status)}: ${printable(item.changes)}`, attention }
  if (kind === 'step_update') return { text: text(step.text_delta) || (tool ? `Tool: ${tool} · ${text(step.state)}\n${printable(object(step.tool_info).parameters)}` : `${text(step.step_type)} · ${text(step.state)}`), attention }
  if (kind === 'result') return { text: typeof event.result === 'string' ? event.result : text(object(event.result).response) || text(object(event.result).error) || printable(event.errors ?? event.subtype), attention }
  if (kind === 'system' && event.subtype === 'init') return { text: `Started ${text(event.model)} in ${text(event.cwd)}`, attention }
  return { text: line, attention }
}
