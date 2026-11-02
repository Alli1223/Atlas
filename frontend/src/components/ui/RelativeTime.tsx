import { absoluteTime, relativeTime } from '@/lib/time'

/** A `<time>` that reads relatively and shows the absolute time on hover. */
export function RelativeTime({ iso }: { iso: string }) {
  return (
    <time dateTime={iso} title={absoluteTime(iso)}>
      {relativeTime(iso)}
    </time>
  )
}
