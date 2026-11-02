import { useNavigate } from '@tanstack/react-router'
import { Bell } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import { Button, EmptyState, RelativeTime, Spinner } from '@/components/ui'
import { cx } from '@/lib/cx'
import { ICON } from '@/lib/icon'
import kit from '@/styles/kit.module.css'

import type { Notification } from './api'
import { useInbox, useMarkAllRead, useMarkRead } from './queries'

/** The bell: unread count, newest-first list, mark read, follow the link. */
export function NotificationBell() {
  const [open, setOpen] = useState(false)
  const inbox = useInbox()
  const markRead = useMarkRead()
  const markAll = useMarkAllRead()
  const navigate = useNavigate()
  const ref = useRef<HTMLDivElement>(null)
  const unread = inbox.data?.unread ?? 0

  useEffect(() => {
    if (!open) return
    const onDown = (event: MouseEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) setOpen(false)
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  const follow = (item: Notification) => {
    if (!item.readAt) markRead.mutate(item.id)
    setOpen(false)
    if (item.cardKey) void navigate({ to: '/cards/$key', params: { key: item.cardKey } })
    else if (item.link) void navigate({ to: item.link })
  }

  return (
    <div className={kit.anchor} ref={ref}>
      <Button
        appearance="subtle"
        isIconOnly
        aria-label={unread > 0 ? `Notifications, ${unread} unread` : 'Notifications'}
        aria-expanded={open}
        iconBefore={<Bell {...ICON} aria-hidden="true" />}
        onClick={() => setOpen((value) => !value)}
      />
      {unread > 0 && <span className={kit.badge}>{unread > 99 ? '99+' : unread}</span>}
      {open && (
        <div className={kit.popover} role="dialog" aria-label="Notifications">
          <div className={kit.header}>
            <h2 className={kit.subtitle}>Notifications</h2>
            <span className={kit.spacer} />
            {unread > 0 && (
              <Button appearance="link" size="compact" onClick={() => markAll.mutate()}>
                Mark all read
              </Button>
            )}
          </div>
          {inbox.isPending ? (
            <Spinner />
          ) : (inbox.data?.items.length ?? 0) === 0 ? (
            <EmptyState isCompact header="You're all caught up" />
          ) : (
            <ul className={kit.list}>
              {inbox.data?.items.map((item) => (
                <li key={item.id}>
                  <button
                    type="button"
                    className={cx(kit.option, !item.readAt && kit.unread)}
                    onClick={() => follow(item)}
                  >
                    <span className={kit.grow}>
                      {item.title}
                      {item.count > 1 ? ` (${item.count})` : ''}
                      <br />
                      <span className={kit.muted}>
                        {item.cardKey ? `${item.cardKey} · ` : ''}
                        <RelativeTime iso={item.updatedAt} />
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}
