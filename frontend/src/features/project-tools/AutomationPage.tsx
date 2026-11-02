import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { Banner, Button, EmptyState, Input, Lozenge, RelativeTime, Spinner } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { type ApiError, api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'
import kit from '@/styles/kit.module.css'

type RuleSpec = components['schemas']['RuleSpec']

async function fetchRules(projectKey: string) {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/automation/rules', { params: { path: { key: projectKey } } }),
  )
}

/** As the list endpoint returns it (the generated request and response shapes differ). */
type Rule = Awaited<ReturnType<typeof fetchRules>>[number]

const EXAMPLE: RuleSpec = {
  trigger: { type: 'cardTransitioned', to: 'Applied' },
  conditions: [],
  actions: [
    { type: 'edit', dueDate: '{{now.plusDays(7)}}' } as never,
    { type: 'comment', body: 'Follow up with {{card.summary}} on {{now.plusDays(7)}}.' } as never,
  ],
}

const describeTrigger = (spec: { trigger: unknown }) => {
  const t = spec.trigger as { type: string; to?: string | null; intervalMinutes?: number }
  if (t.type === 'cardTransitioned' && t.to) return `moved to ${t.to}`
  if (t.type === 'scheduled') return `every ${t.intervalMinutes ?? '?'} min`
  return t.type.replace(/([A-Z])/g, ' $1').toLowerCase()
}

function RuleEditor({ projectKey, rule, onDone }: { projectKey: string; rule?: Rule; onDone: () => void }) {
  const [name, setName] = useState(rule?.name ?? 'Follow up a week after applying')
  const [spec, setSpec] = useState(JSON.stringify((rule?.spec as unknown) ?? EXAMPLE, null, 2))
  const [cardKey, setCardKey] = useState('')
  const [trace, setTrace] = useState<string | undefined>()
  const queryClient = useQueryClient()
  const parse = (): RuleSpec => JSON.parse(spec) as RuleSpec

  const save = useMutation<unknown, ApiError>({
    mutationFn: async () => {
      const body = { name, spec: parse(), enabled: rule?.enabled ?? true, description: rule?.description ?? null }
      return rule
        ? unwrap(await api.PUT('/api/v1/automation/rules/{id}', { params: { path: { id: rule.id } }, body }))
        : unwrap(await api.POST('/api/v1/projects/{key}/automation/rules', { params: { path: { key: projectKey } }, body }))
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['automation', projectKey] })
      toast('success', 'Rule saved.')
      onDone()
    },
    onError: (error) => toast('error', error instanceof SyntaxError ? `Not valid JSON: ${error.message}` : error.message),
  })
  const dry = useMutation<unknown, ApiError>({
    mutationFn: async () =>
      unwrap(
        await api.POST('/api/v1/projects/{key}/automation/dry-run', {
          params: { path: { key: projectKey } },
          body: { spec: parse(), cardKey: cardKey.trim() || null },
        }),
      ),
    onSuccess: (result) => setTrace(JSON.stringify(result, null, 2)),
    onError: (error) => setTrace(error.message),
  })

  return (
    <section className={kit.section} aria-label="Rule editor">
      <h2 className={kit.subtitle}>{rule ? `Edit “${rule.name}”` : 'New rule'}</h2>
      <Input aria-label="Rule name" value={name} onChange={(e) => setName(e.target.value)} />
      <p className={kit.muted}>
        One trigger, optional conditions, then actions. Smart values work as in Jira:{' '}
        <code>{'{{card.key}}'}</code>, <code>{'{{card.assignee.displayName}}'}</code>, <code>{'{{now.plusDays(7)}}'}</code>.
      </p>
      <textarea className={kit.textarea} style={{ minHeight: 260 }} aria-label="Rule specification (JSON)" value={spec} onChange={(e) => setSpec(e.target.value)} spellCheck={false} />
      <div className={kit.row}>
        <Button appearance="primary" onClick={() => save.mutate()} isLoading={save.isPending}>
          Save
        </Button>
        <Button appearance="subtle" onClick={onDone}>
          Cancel
        </Button>
        <span className={kit.spacer} />
        <Input size="compact" aria-label="Card for the dry run" placeholder="Card key for a dry run" value={cardKey} onChange={(e) => setCardKey(e.target.value)} />
        <Button onClick={() => dry.mutate()} isLoading={dry.isPending}>
          Dry run
        </Button>
      </div>
      {trace && <pre className={kit.textarea} style={{ minHeight: 0, whiteSpace: 'pre-wrap' }}>{trace}</pre>}
    </section>
  )
}

function Runs({ rule }: { rule: Rule }) {
  const runs = useQuery({
    queryKey: ['automation', 'runs', rule.id],
    queryFn: async () => unwrap(await api.GET('/api/v1/automation/rules/{id}/runs', { params: { path: { id: rule.id } } })),
  })
  if (runs.isPending) return <Spinner />
  if (!runs.data?.length) return <p className={kit.muted}>No runs yet.</p>
  return (
    <table className={kit.table} aria-label={`Audit log for ${rule.name}`}>
      <thead>
        <tr>
          <th>When</th>
          <th>Trigger</th>
          <th>Card</th>
          <th>Result</th>
          <th>Trace</th>
        </tr>
      </thead>
      <tbody>
        {runs.data.map((run) => (
          <tr key={run.id}>
            <td>
              <RelativeTime iso={run.startedAt} />
            </td>
            <td>{run.trigger}</td>
            <td>{run.cardKey ?? '—'}</td>
            <td>
              <Lozenge appearance={run.status === 'success' ? 'success' : run.status === 'noop' ? 'default' : 'removed'}>{run.status}</Lozenge>
              {run.error && <div className={kit.error}>{run.error}</div>}
            </td>
            <td>
              <details>
                <summary>Show</summary>
                <pre style={{ whiteSpace: 'pre-wrap', margin: 0 }}>{JSON.stringify(run.trace, null, 2)}</pre>
              </details>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** A project's automation rules (`TODO.md` Phase 15): list, toggle, edit, dry run, audit. */
export function AutomationPage({ projectKey }: { projectKey: string }) {
  const [editing, setEditing] = useState<Rule | 'new' | undefined>()
  const [open, setOpen] = useState<string | undefined>()
  const queryClient = useQueryClient()
  const rules = useQuery({
    queryKey: ['automation', projectKey],
    queryFn: () => fetchRules(projectKey),
  })
  const toggle = useMutation<unknown, ApiError, Rule>({
    mutationFn: async (rule) =>
      unwrap(
        await api.PUT('/api/v1/automation/rules/{id}', {
          params: { path: { id: rule.id } },
          body: { name: rule.name, description: rule.description ?? null, spec: rule.spec as never, enabled: !rule.enabled },
        }),
      ),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['automation', projectKey] }),
    onError: (error) => toast('error', error.message),
  })
  const remove = useMutation<unknown, ApiError, Rule>({
    mutationFn: async (rule) => unwrap(await api.DELETE('/api/v1/automation/rules/{id}', { params: { path: { id: rule.id } } })),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['automation', projectKey] }),
    onError: (error) => toast('error', error.message),
  })

  return (
    <div className={kit.page}>
      <div className={kit.header}>
        <h1 className={kit.title}>Automation</h1>
        <span className={kit.spacer} />
        <Button appearance="primary" onClick={() => setEditing('new')}>
          New rule
        </Button>
      </div>
      {editing && (
        <RuleEditor projectKey={projectKey} {...(editing === 'new' ? {} : { rule: editing })} onDone={() => setEditing(undefined)} />
      )}
      {rules.isError && <Banner appearance="error">{(rules.error as ApiError).message}</Banner>}
      {rules.data?.length === 0 && <EmptyState header="No rules yet" description="Rules run on their own when a card changes, on a schedule, or from a webhook." />}
      <ul className={kit.list}>
        {rules.data?.map((rule) => (
          <li key={rule.id} className={kit.section}>
            <div className={kit.row}>
              <strong className={kit.grow}>{rule.name}</strong>
              <span className={kit.muted}>when {describeTrigger(rule.spec)}</span>
              <Lozenge appearance={rule.enabled ? 'success' : 'default'}>{rule.enabled ? 'on' : 'off'}</Lozenge>
              <Button size="compact" onClick={() => toggle.mutate(rule)}>
                {rule.enabled ? 'Turn off' : 'Turn on'}
              </Button>
              <Button size="compact" appearance="subtle" onClick={() => setEditing(rule)}>
                Edit
              </Button>
              <Button size="compact" appearance="subtle" onClick={() => setOpen(open === rule.id ? undefined : rule.id)}>
                {open === rule.id ? 'Hide log' : 'Audit log'}
              </Button>
              <Button size="compact" appearance="subtle" onClick={() => remove.mutate(rule)}>
                Delete
              </Button>
            </div>
            {rule.lastRunAt && (
              <span className={kit.muted}>
                Last ran <RelativeTime iso={rule.lastRunAt} />
              </span>
            )}
            {open === rule.id && <Runs rule={rule} />}
          </li>
        ))}
      </ul>
    </div>
  )
}
