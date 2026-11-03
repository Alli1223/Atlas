import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Banner, Button } from '@/components/ui'
import { cardHref } from '@/features/card-detail/autolink'
import { cardKeys } from '@/features/card-detail/queries'
import { boardKeys } from '@/features/board/queries'
import { addDependency, fetchCardAutomation, removeDependency } from './api'
import styles from './Automation.module.css'

export function CardAutomation({ cardKey }: { cardKey: string }) {
  const client = useQueryClient()
  const [key, setKey] = useState('')
  const [reason, setReason] = useState('')
  const query = useQuery({ queryKey: ['card-automation', cardKey], queryFn: () => fetchCardAutomation(cardKey), refetchInterval: 2000 })
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ['card-automation', cardKey] })
    void client.invalidateQueries({ queryKey: cardKeys.all })
    void client.invalidateQueries({ queryKey: boardKeys.all })
  }
  const add = useMutation({ mutationFn: () => addDependency(cardKey, key.trim().toUpperCase(), reason.trim()), onSuccess: () => { setKey(''); setReason(''); refresh() } })
  const remove = useMutation({ mutationFn: (key: string) => removeDependency(cardKey, key), onSuccess: refresh })
  return <section className={styles.panel} aria-label="Agent assignment and dependencies">
    {query.data?.assignment && <p><strong>Assigned agent: {query.data.assignment.provider}</strong><br />{query.data.assignment.reason}</p>}
    <strong>Blocking tickets</strong>
    {query.isError && <Banner appearance="error">Could not load dependencies.</Banner>}
    {query.data?.blockedBy.length === 0 && <p>No blocking tickets.</p>}
    <ul>{query.data?.blockedBy.map(blocker => <li key={blocker.key}>
      <a href={cardHref(blocker.key)}>{blocker.key}</a> · {blocker.summary} · {blocker.done ? 'Done' : 'Blocking'}
      <p>{blocker.reason}</p>
      <Button size="compact" appearance="subtle" onClick={() => remove.mutate(blocker.key)} isLoading={remove.isPending && remove.variables === blocker.key}>Remove dependency</Button>
    </li>)}</ul>
    <details><summary>Add blocking ticket</summary>
      <form onSubmit={event => { event.preventDefault(); add.mutate() }} className={styles.form}>
        <label>Blocking ticket key<input value={key} onChange={event => setKey(event.target.value)} placeholder="ATLAS-123" required /></label>
        <label>Why is it blocking?<textarea value={reason} onChange={event => setReason(event.target.value)} required /></label>
        <Button type="submit" disabled={!key.trim() || !reason.trim()} isLoading={add.isPending}>Add dependency</Button>
      </form>
    </details>
    {(add.error ?? remove.error) && <Banner appearance="error">{(add.error ?? remove.error)?.message}</Banner>}
  </section>
}
