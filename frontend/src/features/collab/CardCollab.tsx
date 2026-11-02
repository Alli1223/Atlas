import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Eye, EyeOff, Link2, Paperclip, Timer, Trash2, Upload, Sliders } from 'lucide-react'
import { type ClipboardEvent, type DragEvent, useRef, useState } from 'react'

import { Button, Input, Lozenge, RelativeTime, Select, Spinner } from '@/components/ui'
import type { StatusCategory } from '@/components/ui'
import { toast } from '@/features/board/toast'
import { useCurrentUser } from '@/features/auth/queries'
import type { ApiError } from '@/lib/api'
import { ICON } from '@/lib/icon'
import { formatBytes, formatMinutes } from '@/lib/time'
import kit from '@/styles/kit.module.css'

import * as collab from './api'
import type { CardFieldValue, LinkType } from './api'

const keys = {
  watchers: (k: string) => ['collab', 'watchers', k] as const,
  worklogs: (k: string) => ['collab', 'worklogs', k] as const,
  links: (k: string) => ['collab', 'links', k] as const,
  remote: (k: string) => ['collab', 'remote', k] as const,
  attachments: (k: string) => ['collab', 'attachments', k] as const,
  fields: (k: string) => ['collab', 'fields', k] as const,
}

const LINK_TYPES: { value: LinkType; label: string }[] = [
  { value: 'blocks', label: 'blocks' },
  { value: 'is_blocked_by', label: 'is blocked by' },
  { value: 'relates_to', label: 'relates to' },
  { value: 'duplicates', label: 'duplicates' },
  { value: 'is_duplicated_by', label: 'is duplicated by' },
  { value: 'clones', label: 'clones' },
  { value: 'causes', label: 'causes' },
  { value: 'is_caused_by', label: 'is caused by' },
]

/** A mutation that refreshes one query and reports failure as a toast. */
function useAction<T>(queryKey: readonly unknown[], fn: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient()
  return useMutation<unknown, ApiError, T>({
    mutationFn: fn,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey }),
    onError: (error) => toast('error', error.message),
  })
}

function Watchers({ cardKey }: { cardKey: string }) {
  const { user } = useCurrentUser()
  const list = useQuery({ queryKey: keys.watchers(cardKey), queryFn: () => collab.listWatchers(cardKey) })
  const watch = useAction(keys.watchers(cardKey), () => collab.watch(cardKey))
  const unwatch = useAction(keys.watchers(cardKey), (id: string) => collab.unwatch(cardKey, id))
  const watching = list.data?.some((w) => w.userId === user?.id) ?? false
  return (
    <section className={kit.section} aria-label="Watchers">
      <h3 className={kit.sectionTitle}>
        <Eye {...ICON} aria-hidden="true" /> Watchers ({list.data?.length ?? 0})
        <span className={kit.spacer} />
        <Button
          size="compact"
          appearance="subtle"
          iconBefore={watching ? <EyeOff {...ICON} aria-hidden="true" /> : <Eye {...ICON} aria-hidden="true" />}
          onClick={() => (watching && user ? unwatch.mutate(user.id) : watch.mutate(undefined))}
        >
          {watching ? 'Stop watching' : 'Watch'}
        </Button>
      </h3>
      <div className={kit.muted}>
        {list.data && list.data.length > 0 ? list.data.map((w) => w.displayName).join(', ') : 'Nobody yet.'}
      </div>
    </section>
  )
}

function WorkLog({ cardKey }: { cardKey: string }) {
  const [duration, setDuration] = useState('')
  const [note, setNote] = useState('')
  const summary = useQuery({ queryKey: keys.worklogs(cardKey), queryFn: () => collab.listWorklogs(cardKey) })
  const log = useAction(keys.worklogs(cardKey), () => collab.logWork(cardKey, duration, note))
  const remove = useAction(keys.worklogs(cardKey), collab.deleteWorklog)
  return (
    <section className={kit.section} aria-label="Work log">
      <h3 className={kit.sectionTitle}>
        <Timer {...ICON} aria-hidden="true" /> Work log · {summary.data?.totalDisplay ?? '0m'}
      </h3>
      <form
        className={kit.row}
        onSubmit={(event) => {
          event.preventDefault()
          if (!duration.trim()) return
          log.mutate(undefined, {
            onSuccess: () => {
              setDuration('')
              setNote('')
            },
          })
        }}
      >
        <Input size="compact" placeholder="2h 30m" aria-label="Time spent" value={duration} onChange={(e) => setDuration(e.target.value)} />
        <Input size="compact" placeholder="What was done (optional)" aria-label="Work note" value={note} onChange={(e) => setNote(e.target.value)} />
        <Button size="compact" type="submit" isLoading={log.isPending}>
          Log
        </Button>
      </form>
      <ul className={kit.list}>
        {summary.data?.entries.map((entry) => (
          <li key={entry.id} className={kit.item}>
            <strong>{formatMinutes(entry.minutes)}</strong>
            <span className={kit.grow}>{entry.note ?? <span className={kit.muted}>{entry.source}</span>}</span>
            <span className={kit.muted}>
              <RelativeTime iso={entry.startedAt} />
            </span>
            <Button size="compact" appearance="subtle" isIconOnly aria-label="Delete entry" iconBefore={<Trash2 {...ICON} aria-hidden="true" />} onClick={() => remove.mutate(entry.id)} />
          </li>
        ))}
      </ul>
    </section>
  )
}

function Links({ cardKey }: { cardKey: string }) {
  const [target, setTarget] = useState('')
  const [type, setType] = useState<LinkType>('relates_to')
  const [url, setUrl] = useState('')
  const [title, setTitle] = useState('')
  const links = useQuery({ queryKey: keys.links(cardKey), queryFn: () => collab.listLinks(cardKey) })
  const remote = useQuery({ queryKey: keys.remote(cardKey), queryFn: () => collab.listRemoteLinks(cardKey) })
  const add = useAction(keys.links(cardKey), () => collab.linkCard(cardKey, target, type))
  const remove = useAction(keys.links(cardKey), (id: string) => collab.unlinkCard(cardKey, id))
  const addRemote = useAction(keys.remote(cardKey), () => collab.addRemoteLink(cardKey, url, title))
  const removeRemote = useAction(keys.remote(cardKey), (id: string) => collab.removeRemoteLink(cardKey, id))
  return (
    <section className={kit.section} aria-label="Links">
      <h3 className={kit.sectionTitle}>
        <Link2 {...ICON} aria-hidden="true" /> Links
      </h3>
      <ul className={kit.list}>
        {links.data?.map((link) => (
          <li key={link.id} className={kit.item}>
            <span className={kit.muted}>{link.linkType.replace(/_/g, ' ')}</span>
            <Link to="/cards/$key" params={{ key: link.cardKey }}>
              {link.cardKey}
            </Link>
            <span className={kit.grow}>{link.cardSummary}</span>
            <Lozenge statusCategory={link.statusCategory as StatusCategory}>{link.statusName}</Lozenge>
            <Button size="compact" appearance="subtle" isIconOnly aria-label={`Unlink ${link.cardKey}`} iconBefore={<Trash2 {...ICON} aria-hidden="true" />} onClick={() => remove.mutate(link.id)} />
          </li>
        ))}
        {remote.data?.map((link) => (
          <li key={link.id} className={kit.item}>
            <span className={kit.muted}>web</span>
            <a className={kit.grow} href={link.url} target="_blank" rel="noopener noreferrer">
              {link.title}
            </a>
            <Button size="compact" appearance="subtle" isIconOnly aria-label={`Remove ${link.title}`} iconBefore={<Trash2 {...ICON} aria-hidden="true" />} onClick={() => removeRemote.mutate(link.id)} />
          </li>
        ))}
      </ul>
      <form
        className={kit.row}
        onSubmit={(event) => {
          event.preventDefault()
          if (target.trim()) add.mutate(undefined, { onSuccess: () => setTarget('') })
        }}
      >
        <Select aria-label="Link type" options={LINK_TYPES} value={type} onChange={(e) => setType(e.target.value as LinkType)} />
        <Input size="compact" placeholder="Card key, e.g. JOB-12" aria-label="Card to link" value={target} onChange={(e) => setTarget(e.target.value)} />
        <Button size="compact" type="submit">
          Link card
        </Button>
      </form>
      <form
        className={kit.row}
        onSubmit={(event) => {
          event.preventDefault()
          if (url.trim())
            addRemote.mutate(undefined, {
              onSuccess: () => {
                setUrl('')
                setTitle('')
              },
            })
        }}
      >
        <Input size="compact" placeholder="https://…" aria-label="Web link URL" value={url} onChange={(e) => setUrl(e.target.value)} />
        <Input size="compact" placeholder="Title (optional)" aria-label="Web link title" value={title} onChange={(e) => setTitle(e.target.value)} />
        <Button size="compact" type="submit">
          Add web link
        </Button>
      </form>
    </section>
  )
}

function Attachments({ cardKey }: { cardKey: string }) {
  const input = useRef<HTMLInputElement>(null)
  const versionFor = useRef<string | null>(null)
  const [dragging, setDragging] = useState(false)
  const list = useQuery({ queryKey: keys.attachments(cardKey), queryFn: () => collab.listAttachments(cardKey) })
  const upload = useAction(keys.attachments(cardKey), (file: File) =>
    versionFor.current ? collab.uploadVersion(versionFor.current, file) : collab.uploadAttachment(cardKey, file),
  )
  const remove = useAction(keys.attachments(cardKey), collab.deleteAttachment)

  const send = (files: FileList | File[]) => {
    for (const file of Array.from(files)) upload.mutate(file)
  }
  const onDrop = (event: DragEvent) => {
    event.preventDefault()
    setDragging(false)
    versionFor.current = null
    send(event.dataTransfer.files)
  }
  const onPaste = (event: ClipboardEvent) => {
    const files = Array.from(event.clipboardData.files)
    if (files.length > 0) {
      event.preventDefault()
      versionFor.current = null
      send(files.map((f, i) => (f.name ? f : new File([f], `pasted-${Date.now()}-${i}.png`, { type: f.type }))))
    }
  }

  return (
    <section
      className={kit.section}
      aria-label="Attachments"
      onDragOver={(e) => {
        e.preventDefault()
        setDragging(true)
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
      onPaste={onPaste}
      tabIndex={-1}
      style={dragging ? { outline: '2px dashed var(--ds-border-focused)' } : undefined}
    >
      <h3 className={kit.sectionTitle}>
        <Paperclip {...ICON} aria-hidden="true" /> Attachments ({list.data?.length ?? 0})
        <span className={kit.spacer} />
        {upload.isPending && <Spinner size="small" />}
        <Button
          size="compact"
          appearance="subtle"
          iconBefore={<Upload {...ICON} aria-hidden="true" />}
          onClick={() => {
            versionFor.current = null
            input.current?.click()
          }}
        >
          Upload
        </Button>
      </h3>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        aria-label="Choose files"
        onChange={(event) => {
          if (event.target.files) send(event.target.files)
          event.target.value = ''
        }}
      />
      <p className={kit.muted}>Drop files here, or paste an image.</p>
      <ul className={kit.list}>
        {list.data?.map((file) => (
          <li key={file.id} className={kit.item}>
            {file.contentType.startsWith('image/') && (
              <img src={collab.attachmentUrl(file.id)} alt="" width={40} height={40} style={{ objectFit: 'cover', borderRadius: 4 }} />
            )}
            <a className={kit.grow} href={collab.attachmentUrl(file.id)} target="_blank" rel="noopener noreferrer">
              {file.filename}
            </a>
            <span className={kit.muted}>
              v{file.currentVersion} · {formatBytes(file.sizeBytes)} · <RelativeTime iso={file.updatedAt} />
            </span>
            <Button
              size="compact"
              appearance="subtle"
              onClick={() => {
                versionFor.current = file.id
                input.current?.click()
              }}
            >
              New version
            </Button>
            <Button size="compact" appearance="subtle" isIconOnly aria-label={`Delete ${file.filename}`} iconBefore={<Trash2 {...ICON} aria-hidden="true" />} onClick={() => remove.mutate(file.id)} />
          </li>
        ))}
      </ul>
    </section>
  )
}

function FieldEditor({ cardKey, item }: { cardKey: string; item: CardFieldValue }) {
  const field = item.entry.field
  const save = useAction(keys.fields(cardKey), (value: unknown) => collab.setField(cardKey, field.id, value))
  const current = item.value
  const label = `${field.name}${item.entry.required ? ' *' : ''}`
  const commit = (raw: string) => {
    if (raw === '') return save.mutate(null)
    if (field.fieldType === 'number') return save.mutate(Number(raw))
    if (field.fieldType === 'multiselect' || field.fieldType === 'labels')
      return save.mutate(raw.split(',').map((v) => v.trim()).filter(Boolean))
    return save.mutate(raw)
  }
  let control
  if (field.fieldType === 'checkbox') {
    control = <input type="checkbox" aria-label={field.name} checked={current === true} onChange={(e) => save.mutate(e.target.checked)} />
  } else if (field.fieldType === 'select' || field.fieldType === 'radio') {
    control = (
      <Select
        aria-label={field.name}
        placeholder="—"
        options={[{ label: '—', value: '' }, ...field.options.map((o) => ({ label: o, value: o }))]}
        value={typeof current === 'string' ? current : ''}
        onChange={(e) => commit(e.target.value)}
      />
    )
  } else {
    const shown = Array.isArray(current)
      ? current.join(', ')
      : typeof current === 'string' || typeof current === 'number'
        ? String(current)
        : ''
    const type = { number: 'number', date: 'date', datetime: 'datetime-local', url: 'url' }[field.fieldType as string] ?? 'text'
    control = (
      <Input
        key={shown}
        size="compact"
        type={type}
        aria-label={field.name}
        defaultValue={shown}
        onBlur={(e) => {
          if (e.target.value !== shown) commit(e.target.value)
        }}
      />
    )
  }
  return (
    <li className={kit.item}>
      <span className={kit.grow} title={field.description ?? undefined}>
        {label}
      </span>
      {control}
    </li>
  )
}

function CustomFields({ cardKey }: { cardKey: string }) {
  const fields = useQuery({ queryKey: keys.fields(cardKey), queryFn: () => collab.listFields(cardKey) })
  const visible = (fields.data ?? []).filter((f) => !f.entry.hidden)
  if (visible.length === 0) return null
  return (
    <section className={kit.section} aria-label="Fields">
      <h3 className={kit.sectionTitle}>
        <Sliders {...ICON} aria-hidden="true" /> Fields
      </h3>
      <ul className={kit.list}>
        {visible.map((item) => (
          <FieldEditor key={item.entry.id} cardKey={cardKey} item={item} />
        ))}
      </ul>
    </section>
  )
}

/** Everything collaborative about a card below its description (`TODO.md` Phase 9). */
export function CardCollab({ cardKey }: { cardKey: string }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--ds-space-150)' }}>
      <CustomFields cardKey={cardKey} />
      <Attachments cardKey={cardKey} />
      <Links cardKey={cardKey} />
      <WorkLog cardKey={cardKey} />
      <Watchers cardKey={cardKey} />
    </div>
  )
}
