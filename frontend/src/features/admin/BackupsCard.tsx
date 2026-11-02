import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { Button, RelativeTime, Spinner } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { type ApiError, api, unwrap } from '@/lib/api'
import { formatBytes } from '@/lib/time'
import kit from '@/styles/kit.module.css'

/**
 * Backups (`TODO.md` Phase 20): what is on disk, a "back up now", and the two files of each
 * to download. Restoring is offline on purpose — `atlas restore <name>` with Atlas stopped.
 */
export function BackupsCard() {
  const queryClient = useQueryClient()
  const backups = useQuery({
    queryKey: ['admin', 'backups'],
    queryFn: async () => unwrap(await api.GET('/api/v1/admin/backups', {})),
  })
  const take = useMutation<unknown, ApiError>({
    mutationFn: async () => unwrap(await api.POST('/api/v1/admin/backups', {})),
    onSuccess: () => {
      toast('success', 'Backup written.')
      void queryClient.invalidateQueries({ queryKey: ['admin', 'backups'] })
    },
    onError: (error) => toast('error', error.message),
  })
  return (
    <section className={kit.section} aria-label="Backups">
      <div className={kit.header}>
        <h2 className={kit.subtitle}>Backups</h2>
        <span className={kit.spacer} />
        <Button onClick={() => take.mutate()} isLoading={take.isPending}>
          Back up now
        </Button>
      </div>
      <p className={kit.muted}>
        Taken automatically every <code>ATLAS_BACKUP_INTERVAL_HOURS</code> (the database by
        <code> VACUUM INTO</code>, uploads as a .tar.gz). To restore, stop Atlas and run{' '}
        <code>atlas restore &lt;name&gt;</code>.
      </p>
      {backups.isPending && <Spinner />}
      <ul className={kit.list}>
        {backups.data?.map((backup) => (
          <li key={backup.name} className={kit.item}>
            <span className={kit.grow}>
              <strong>{backup.name}</strong> · <RelativeTime iso={backup.createdAt} />
            </span>
            <a href={`/api/v1/admin/backups/${backup.name}/database`} download>
              Database ({formatBytes(backup.databaseBytes)})
            </a>
            {backup.uploadsBytes != null && (
              <a href={`/api/v1/admin/backups/${backup.name}/uploads`} download>
                Uploads ({formatBytes(backup.uploadsBytes)})
              </a>
            )}
          </li>
        ))}
      </ul>
    </section>
  )
}
