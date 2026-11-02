import { createFileRoute } from '@tanstack/react-router'

import { SearchPage } from '@/features/search'

interface SearchParams {
  q?: string
}

export const Route = createFileRoute('/search')({
  validateSearch: (search: Record<string, unknown>): SearchParams =>
    typeof search.q === 'string' ? { q: search.q } : {},
  component: SearchRoute,
})

function SearchRoute() {
  const { q } = Route.useSearch()
  const navigate = Route.useNavigate()
  return (
    <SearchPage
      key={q ?? ''}
      initialQuery={q ?? ''}
      onQueryChange={(next) => void navigate({ search: next.trim() ? { q: next } : {} })}
    />
  )
}
