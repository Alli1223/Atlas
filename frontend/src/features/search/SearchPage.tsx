import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Download } from 'lucide-react'
import { useState } from 'react'

import { Banner, Button, Checkbox, EmptyState, Input, RelativeTime, Select, Spinner } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { api, type ApiError, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'
import { ICON } from '@/lib/icon'
import kit from '@/styles/kit.module.css'

type Operation = components['schemas']['Operation']
type Report = components['schemas']['Report']

const KINDS = [
  { value: 'transition', label: 'Move to status…' },
  { value: 'tag', label: 'Add tag…' },
  { value: 'untag', label: 'Remove tag…' },
  { value: 'edit-priority', label: 'Set priority…' },
  { value: 'edit-assignee', label: 'Assign to (username)…' },
  { value: 'archive', label: 'Archive' },
  { value: 'delete', label: 'Move to trash' },
] as const

type Kind = (typeof KINDS)[number]['value']

function operationFor(kind: Kind, value: string): Operation {
  switch (kind) {
    case 'transition':
      return { kind: 'transition', status: value }
    case 'tag':
      return { kind: 'tag', name: value }
    case 'untag':
      return { kind: 'untag', name: value }
    case 'edit-priority':
      return { kind: 'edit', priority: value }
    case 'edit-assignee':
      return { kind: 'edit', assignee: value }
    case 'archive':
      return { kind: 'archive' }
    case 'delete':
      return { kind: 'delete' }
  }
}

/** The operation that undoes one, where one exists. */
function inverse(operation: Operation): Operation | undefined {
  switch (operation.kind) {
    case 'tag':
      return { kind: 'untag', name: operation.name }
    case 'untag':
      return { kind: 'tag', name: operation.name }
    case 'archive':
      return { kind: 'unarchive' }
    case 'delete':
      return { kind: 'restore' }
    default:
      return undefined
  }
}

async function runBulk(keys: string[], operation: Operation, preview: boolean): Promise<Report> {
  return unwrap(await api.POST('/api/v1/bulk', { body: { keys, operation, preview } }))
}

/**
 * AQL search with bulk actions (`TODO.md` Phases 6 and 18): run a query, tick cards, choose an
 * operation, **preview** it (the server runs the real thing and rolls back), then apply. A
 * reversible apply offers Undo in its toast.
 */
export function SearchPage({ initialQuery, onQueryChange }: { initialQuery: string; onQueryChange: (q: string) => void }) {
  const [draft, setDraft] = useState(initialQuery)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [kind, setKind] = useState<Kind>('transition')
  const [value, setValue] = useState('')
  const [preview, setPreview] = useState<Report | undefined>()
  const queryClient = useQueryClient()

  const results = useQuery({
    queryKey: ['search', initialQuery],
    queryFn: async () =>
      unwrap(await api.POST('/api/v1/search', { body: { aql: initialQuery, pageSize: 200 } })),
    enabled: initialQuery.trim().length > 0,
    retry: false,
  })

  const needsValue = !['archive', 'delete'].includes(kind)
  const operation = operationFor(kind, value.trim())
  const keys = [...selected]

  const check = useMutation<Report, ApiError>({
    mutationFn: () => runBulk(keys, operation, true),
    onSuccess: setPreview,
    onError: (error) => toast('error', error.message),
  })
  const apply = useMutation<Report, ApiError>({
    mutationFn: () => runBulk(keys, operation, false),
    onSuccess: (report) => {
      setPreview(undefined)
      setSelected(new Set())
      void queryClient.invalidateQueries({ queryKey: ['search'] })
      void queryClient.invalidateQueries({ queryKey: ['board'] })
      const undo = inverse(operation)
      const changed = report.cards.filter((c) => c.result === 'changed').map((c) => c.key)
      toast(
        report.failed > 0 ? 'error' : 'success',
        `${report.changed} changed, ${report.unchanged} already so, ${report.failed} failed.`,
        undo && changed.length > 0
          ? {
              label: 'Undo',
              run: () =>
                void runBulk(changed, undo, false).then(() => {
                  void queryClient.invalidateQueries({ queryKey: ['search'] })
                  toast('info', 'Undone.')
                }),
            }
          : undefined,
      )
    },
    onError: (error) => toast('error', error.message),
  })

  const cards = results.data?.cards ?? []
  const allSelected = cards.length > 0 && cards.every((c) => selected.has(c.key))

  return (
    <div className={kit.page}>
      <div className={kit.header}>
        <h1 className={kit.title}>Search</h1>
      </div>
      <form
        className={kit.row}
        onSubmit={(event) => {
          event.preventDefault()
          setSelected(new Set())
          setPreview(undefined)
          onQueryChange(draft)
        }}
      >
        <div style={{ flex: 1 }}>
          <Input
            aria-label="AQL query"
            placeholder='project = JOB AND status != Done ORDER BY updated DESC'
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            autoFocus
          />
        </div>
        <Button type="submit" appearance="primary">
          Search
        </Button>
        {initialQuery && (
          <a
            href={`/api/v1/export/csv?aql=${encodeURIComponent(initialQuery)}`}
            download
            className={kit.row}
            aria-label="Export results as CSV"
          >
            <Download {...ICON} aria-hidden="true" /> CSV
          </a>
        )}
      </form>

      {results.isError && <Banner appearance="error">{(results.error as ApiError).message}</Banner>}
      {results.isFetching && <Spinner />}

      {selected.size > 0 && (
        <div className={kit.section} aria-label="Bulk change">
          <div className={kit.row}>
            <strong>{selected.size} selected</strong>
            <Select
              aria-label="Bulk operation"
              options={KINDS.map((k) => ({ value: k.value, label: k.label }))}
              value={kind}
              onChange={(e) => {
                setKind(e.target.value as Kind)
                setPreview(undefined)
              }}
            />
            {needsValue && (
              <Input size="compact" aria-label="Value" placeholder="Value" value={value} onChange={(e) => {
                setValue(e.target.value)
                setPreview(undefined)
              }} />
            )}
            <Button onClick={() => check.mutate()} isLoading={check.isPending} disabled={needsValue && !value.trim()}>
              Preview
            </Button>
            <Button
              appearance={kind === 'delete' ? 'danger' : 'primary'}
              onClick={() => apply.mutate()}
              isLoading={apply.isPending}
              disabled={!preview || preview.changed === 0}
            >
              Apply
            </Button>
          </div>
          {preview && (
            <div className={kit.muted} role="status">
              Preview: {preview.changed} would change, {preview.unchanged} already so, {preview.failed} would fail.
              {preview.cards
                .filter((c) => c.result === 'failed')
                .slice(0, 10)
                .map((c) => (
                  <div key={c.key} className={kit.error}>
                    {c.key}: {c.error}
                  </div>
                ))}
            </div>
          )}
        </div>
      )}

      {results.data && cards.length === 0 && <EmptyState header="No cards match" />}
      {cards.length > 0 && (
        <>
          <div className={kit.muted}>
            {results.data?.total} match{results.data?.total === 1 ? '' : 'es'}
            {results.data && results.data.total > cards.length ? ` (showing ${cards.length})` : ''}
          </div>
          <table className={kit.table}>
            <thead>
              <tr>
                <th>
                  <Checkbox
                    aria-label="Select all"
                    checked={allSelected}
                    onChange={() => setSelected(allSelected ? new Set() : new Set(cards.map((c) => c.key)))}
                  />
                </th>
                <th>Key</th>
                <th>Summary</th>
                <th>Due</th>
                <th>Updated</th>
              </tr>
            </thead>
            <tbody>
              {cards.map((card) => (
                <tr key={card.id}>
                  <td>
                    <Checkbox
                      aria-label={`Select ${card.key}`}
                      checked={selected.has(card.key)}
                      onChange={() => {
                        const next = new Set(selected)
                        if (next.has(card.key)) next.delete(card.key)
                        else next.add(card.key)
                        setSelected(next)
                        setPreview(undefined)
                      }}
                    />
                  </td>
                  <td>
                    <Link to="/cards/$key" params={{ key: card.key }}>
                      {card.key}
                    </Link>
                  </td>
                  <td>{card.summary}</td>
                  <td>{card.dueDate ?? ''}</td>
                  <td className={kit.muted}>
                    <RelativeTime iso={card.updatedAt} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
    </div>
  )
}
