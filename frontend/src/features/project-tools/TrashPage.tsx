import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { Button, EmptyState, RelativeTime, Spinner } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { type ApiError, api, unwrap } from '@/lib/api'
import kit from '@/styles/kit.module.css'

/** A project's trash (`TODO.md` Phase 18): soft-deleted cards, restorable one by one or all at once. */
export function TrashPage({ projectKey }: { projectKey: string }) {
  const queryClient = useQueryClient()
  const trash = useQuery({
    queryKey: ['trash', projectKey],
    queryFn: async () => unwrap(await api.GET('/api/v1/projects/{key}/trash', { params: { path: { key: projectKey } } })),
  })
  const restore = useMutation<unknown, ApiError, string[]>({
    mutationFn: async (keys) =>
      unwrap(await api.POST('/api/v1/bulk', { body: { keys, operation: { kind: 'restore' }, preview: false } })),
    onSuccess: (_, keys) => {
      toast('success', `Restored ${keys.length} card${keys.length === 1 ? '' : 's'}.`)
      void queryClient.invalidateQueries({ queryKey: ['trash', projectKey] })
      void queryClient.invalidateQueries({ queryKey: ['board'] })
    },
    onError: (error) => toast('error', error.message),
  })
  const cards = trash.data ?? []
  return (
    <div className={kit.page}>
      <div className={kit.header}>
        <h1 className={kit.title}>Trash</h1>
        <span className={kit.spacer} />
        {cards.length > 0 && (
          <Button onClick={() => restore.mutate(cards.map((c) => c.key))} isLoading={restore.isPending}>
            Restore all
          </Button>
        )}
      </div>
      {trash.isPending && <Spinner />}
      {trash.data && cards.length === 0 && (
        <EmptyState header="The trash is empty" description="Deleted cards wait here, keys and all, until restored." />
      )}
      <ul className={kit.list}>
        {cards.map((card) => (
          <li key={card.id} className={kit.item}>
            <strong>{card.key}</strong>
            <span className={kit.grow}>{card.summary}</span>
            <span className={kit.muted}>
              updated <RelativeTime iso={card.updatedAt} />
            </span>
            <Button size="compact" onClick={() => restore.mutate([card.key])}>
              Restore
            </Button>
          </li>
        ))}
      </ul>
    </div>
  )
}
