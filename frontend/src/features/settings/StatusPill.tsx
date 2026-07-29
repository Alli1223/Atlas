import { Lozenge } from '@/components/ui'

import type { PillStatus } from './api'
import { STATUS_APPEARANCE, STATUS_LABEL } from './status'

export interface StatusPillProps {
  status: PillStatus
}

/**
 * The per-key status pill: valid (green), expiring (yellow), expired/invalid (red),
 * unchecked (grey).
 *
 * A thin wrapper over [`Lozenge`] whose only job is the status → appearance mapping, kept
 * in [`STATUS_APPEARANCE`] so the colour of a status is decided in one place and asserted
 * by one test. `data-status` surfaces the raw status in the DOM as a stable hook for that
 * test — the Lozenge's own class names are hashed CSS modules and are not a contract.
 */
export function StatusPill({ status }: StatusPillProps) {
  return (
    <Lozenge appearance={STATUS_APPEARANCE[status]} data-status={status}>
      {STATUS_LABEL[status]}
    </Lozenge>
  )
}
