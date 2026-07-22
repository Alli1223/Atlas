import { createFileRoute } from '@tanstack/react-router'

import { CardDetail, cardQueryOptions } from '@/features/card-detail'

import styles from './cards.$key.module.css'

/**
 * The deep-linkable card page: `/cards/ATLAS-123`.
 *
 * The full-page half of "a modal AND a full-page route" — the board opens the modal, but the
 * URL, a bookmark, or an `ATLAS-123` autolink resolves here, and both render the exact same
 * [`CardDetail`] body. The loader warms the card query the component reads, so a hard nav to
 * this URL has the card in cache before the first paint rather than flashing a spinner.
 */
export const Route = createFileRoute('/cards/$key')({
  loader: ({ context, params }) => {
    // Uppercased to match the backend's case-insensitive key lookup, so `/cards/atlas-1`
    // and `/cards/ATLAS-1` warm and read one cache entry rather than two.
    void context.queryClient.ensureQueryData(cardQueryOptions(params.key.toUpperCase()))
  },
  component: CardRoute,
})

function CardRoute() {
  const { key } = Route.useParams()
  return (
    <div className={styles.page}>
      <CardDetail cardKey={key.toUpperCase()} />
    </div>
  )
}
