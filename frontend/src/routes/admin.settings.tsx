import { createFileRoute } from '@tanstack/react-router'

import { AdminSettingsPage } from '@/features/admin'

export const Route = createFileRoute('/admin/settings')({
  component: AdminSettingsRoute,
})

function AdminSettingsRoute() {
  return <AdminSettingsPage />
}
