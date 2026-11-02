import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { useState } from 'react'

import { Banner, Button, Checkbox, Input, Select } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { type ApiError, api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'
import kit from '@/styles/kit.module.css'

type Plan = components['schemas']['Plan']
type Target = components['schemas']['Target']

const TARGETS: { value: string; label: string }[] = [
  { value: 'skip', label: 'Skip' },
  { value: 'summary', label: 'Summary' },
  { value: 'description', label: 'Description' },
  { value: 'type', label: 'Type' },
  { value: 'status', label: 'Status' },
  { value: 'priority', label: 'Priority' },
  { value: 'assignee', label: 'Assignee' },
  { value: 'reporter', label: 'Reporter' },
  { value: 'labels', label: 'Labels' },
  { value: 'dueDate', label: 'Due date' },
  { value: 'startDate', label: 'Start date' },
  { value: 'estimate', label: 'Estimate' },
  { value: 'milestone', label: 'Milestone' },
  { value: 'component', label: 'Component' },
  { value: 'issueId', label: 'Issue ID' },
  { value: 'parent', label: 'Parent' },
]

/** A target as the select shows it: custom fields as `field:<id>`. */
const targetValue = (target: Target): string =>
  typeof target === 'string' ? target : `field:${target.customField}`
const targetFrom = (value: string): Target =>
  (value.startsWith('field:') ? { customField: value.slice(6) } : value) as Target

function CsvWizard({ projectKey }: { projectKey: string }) {
  const [csv, setCsv] = useState('')
  const [plan, setPlan] = useState<Plan | undefined>()
  const [mapping, setMapping] = useState<Target[] | undefined>()
  const queryClient = useQueryClient()

  const preview = useMutation<Plan, ApiError, Target[] | undefined>({
    mutationFn: async (override) =>
      unwrap(
        await api.POST('/api/v1/projects/{key}/import/csv/preview', {
          params: { path: { key: projectKey } },
          body: { csv, mapping: override ?? null },
        }),
      ),
    onSuccess: (result) => {
      setPlan(result)
      setMapping(result.mapping)
    },
    onError: (error) => toast('error', error.message),
  })
  const run = useMutation<{ keys: string[] }, ApiError>({
    mutationFn: async () => {
      const response = await api.POST('/api/v1/projects/{key}/import/csv', {
        params: { path: { key: projectKey } },
        body: { csv, mapping: mapping ?? null },
      })
      if (response.response.status === 422 && response.error && 'problems' in (response.error as object)) {
        setPlan(response.error)
        throw new Error('The file has problems; nothing was imported.')
      }
      return unwrap(response)
    },
    onSuccess: (result) => {
      toast('success', `Imported ${result.keys.length} cards.`)
      setCsv('')
      setPlan(undefined)
      void queryClient.invalidateQueries({ queryKey: ['board'] })
    },
    onError: (error) => toast('error', error.message),
  })

  return (
    <section className={kit.section} aria-label="Import CSV">
      <h2 className={kit.subtitle}>Import a spreadsheet (CSV)</h2>
      <p className={kit.muted}>
        <strong>Summary</strong> is the only required column. Repeat a header (three <em>Labels</em> columns) for several
        values; <em>Issue ID</em> + <em>Parent</em> build the hierarchy. Every row is checked first — a file imports
        completely or not at all.
      </p>
      <input
        type="file"
        accept=".csv,text/csv"
        aria-label="CSV file"
        onChange={(event) => {
          const file = event.target.files?.[0]
          if (file) void file.text().then((text) => {
            setCsv(text)
            setPlan(undefined)
          })
        }}
      />
      <textarea className={kit.textarea} aria-label="CSV text" placeholder="…or paste CSV here" value={csv} onChange={(e) => {
        setCsv(e.target.value)
        setPlan(undefined)
      }} />
      <div className={kit.row}>
        <Button onClick={() => preview.mutate(undefined)} isLoading={preview.isPending} disabled={!csv.trim()}>
          Preview
        </Button>
        <Button appearance="primary" onClick={() => run.mutate()} isLoading={run.isPending} disabled={!plan || plan.problems.length > 0}>
          Import {plan ? `${plan.total} cards` : ''}
        </Button>
      </div>
      {plan && (
        <>
          <table className={kit.table} aria-label="Column mapping">
            <thead>
              <tr>
                <th>Column</th>
                <th>Becomes</th>
              </tr>
            </thead>
            <tbody>
              {plan.headers.map((header, i) => (
                <tr key={header + String(i)}>
                  <td>{header}</td>
                  <td>
                    <Select
                      aria-label={`Mapping for ${header}`}
                      options={[
                        ...TARGETS,
                        ...(mapping?.[i] && typeof mapping[i] !== 'string'
                          ? [{ value: targetValue(mapping[i]), label: 'Custom field' }]
                          : []),
                      ]}
                      value={mapping?.[i] ? targetValue(mapping[i]) : 'skip'}
                      onChange={(e) => {
                        const next = [...(mapping ?? [])]
                        next[i] = targetFrom(e.target.value)
                        setMapping(next)
                        preview.mutate(next)
                      }}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {plan.problems.length > 0 ? (
            <Banner appearance="error">
              {plan.problems.length} problem{plan.problems.length === 1 ? '' : 's'} — fix the file or the mapping:
              <ul>
                {plan.problems.slice(0, 20).map((p, i) => (
                  <li key={String(i)}>
                    Row {p.row}
                    {p.column ? `, ${p.column}` : ''}: {p.message}
                  </li>
                ))}
              </ul>
            </Banner>
          ) : (
            <table className={kit.table} aria-label="Cards to create">
              <thead>
                <tr>
                  <th>Row</th>
                  <th>Summary</th>
                  <th>Type</th>
                  <th>Status</th>
                  <th>Labels</th>
                </tr>
              </thead>
              <tbody>
                {plan.cards.slice(0, 50).map((card) => (
                  <tr key={card.row}>
                    <td>{card.row}</td>
                    <td>{card.summary}</td>
                    <td>{card.cardType}</td>
                    <td>{card.status}</td>
                    <td>{card.labels.join(', ')}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </section>
  )
}

function Bundles({ projectKey }: { projectKey: string }) {
  const [key, setKey] = useState('')
  const [name, setName] = useState('')
  const [withCards, setWithCards] = useState(false)
  const [bundle, setBundle] = useState<unknown>()
  const [importKey, setImportKey] = useState('')
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const opened = (created: { key: string }) => {
    void queryClient.invalidateQueries({ queryKey: ['projects'] })
    toast('success', `Created ${created.key}.`)
    void navigate({ to: '/projects/$projectKey/board', params: { projectKey: created.key } })
  }
  const duplicate = useMutation<{ key: string }, ApiError>({
    mutationFn: async () =>
      unwrap(
        await api.POST('/api/v1/projects/{key}/duplicate', {
          params: { path: { key: projectKey } },
          body: { key, name, includeCards: withCards },
        }),
      ),
    onSuccess: opened,
    onError: (error) => toast('error', error.message),
  })
  const restore = useMutation<{ key: string }, ApiError>({
    mutationFn: async () =>
      unwrap(await api.POST('/api/v1/projects/import', { body: { bundle: bundle as never, key: importKey, includeCards: true } })),
    onSuccess: opened,
    onError: (error) => toast('error', error.message),
  })

  return (
    <section className={kit.section} aria-label="Project bundle">
      <h2 className={kit.subtitle}>Export, copy and restore</h2>
      <div className={kit.row}>
        <a href={`/api/v1/projects/${projectKey}/export`} download={`${projectKey}.atlas.json`}>
          Download the project (JSON, with cards)
        </a>
        <span className={kit.muted}>·</span>
        <a href={`/api/v1/projects/${projectKey}/export?includeCards=false`} download={`${projectKey}.template.atlas.json`}>
          Configuration only (a template)
        </a>
        <span className={kit.muted}>·</span>
        <a href={`/api/v1/export/csv?aql=${encodeURIComponent(`project = ${projectKey}`)}`} download>
          All cards as CSV
        </a>
      </div>
      <h3 className={kit.sectionTitle}>Copy this project's configuration</h3>
      <div className={kit.row}>
        <Input size="compact" aria-label="New project key" placeholder="KEY" value={key} onChange={(e) => setKey(e.target.value.toUpperCase())} />
        <Input size="compact" aria-label="New project name" placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} />
        <Checkbox label="Include cards" checked={withCards} onChange={(e) => setWithCards(e.target.checked)} />
        <Button onClick={() => duplicate.mutate()} isLoading={duplicate.isPending} disabled={!key || !name}>
          Create copy
        </Button>
      </div>
      <h3 className={kit.sectionTitle}>Create a project from a bundle</h3>
      <div className={kit.row}>
        <input
          type="file"
          accept=".json,application/json"
          aria-label="Bundle file"
          onChange={(event) => {
            const file = event.target.files?.[0]
            if (file)
              void file.text().then((text) => {
                try {
                  setBundle(JSON.parse(text))
                } catch {
                  toast('error', 'That file is not JSON.')
                }
              })
          }}
        />
        <Input size="compact" aria-label="Key for the restored project" placeholder="KEY" value={importKey} onChange={(e) => setImportKey(e.target.value.toUpperCase())} />
        <Button onClick={() => restore.mutate()} isLoading={restore.isPending} disabled={!bundle || !importKey}>
          Create project
        </Button>
      </div>
    </section>
  )
}

/** Import and export for one project (`TODO.md` Phase 18). */
export function ImportExportPage({ projectKey }: { projectKey: string }) {
  return (
    <div className={kit.page}>
      <h1 className={kit.title}>Import and export</h1>
      <CsvWizard projectKey={projectKey} />
      <Bundles projectKey={projectKey} />
    </div>
  )
}
