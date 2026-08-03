import { Link } from '@tanstack/react-router'
import { type ReactNode } from 'react'

import { Avatar, Lozenge, type StatusCategory as LozengeCategory } from '@/components/ui'
import { CardCycleField } from '@/features/cycles'
import { TagPicker } from '@/features/tags'
import {
  useAttachTag,
  useCardTags,
  useCreateTag,
  useDetachTag,
  useProjectTags,
  type Tag,
} from '@/features/tags'

import type { Card, StatusCategory } from './api'
import styles from './CardDetail.module.css'
import { Development } from './Development'
import { formatDate, formatDateTime } from './format'
import {
  useCardTypes,
  useMembers,
  usePatchCard,
  usePriorities,
  useResolutions,
  useStatuses,
} from './queries'
import { TransitionButtons } from './TransitionButtons'

export interface SidebarProps {
  card: Card
  projectKey: string
  /** id → card key/summary, so the parent row can link. */
  parentLookup?: Map<string, { key: string; summary: string }> | undefined
}

/** Maps the backend's three categories onto the Lozenge's spelling. */
const CATEGORY: Record<StatusCategory, LozengeCategory> = {
  todo: 'todo',
  in_progress: 'inprogress',
  done: 'done',
}

/**
 * The card's metadata column — status, people, priority, dates, tags, parent.
 *
 * Every editable field here is *inline*: the value shows as text or a lozenge, and editing
 * it is a native control that writes through [`usePatchCard`] optimistically. Native
 * `<select>`s rather than a custom listbox, deliberately — they get keyboard, mobile and
 * screen-reader behaviour for free, and the sidebar is a dense stack of them where that
 * consistency matters more than avatars-in-options would.
 */
export function Sidebar({ card, projectKey, parentLookup }: SidebarProps) {
  const statuses = useStatuses(projectKey)
  const priorities = usePriorities(projectKey)
  const resolutions = useResolutions(projectKey)
  const cardTypes = useCardTypes(projectKey)
  const patch = usePatchCard(card.key)

  const status = statuses.data?.find((s) => s.id === card.statusId)
  const priority = priorities.data?.find((p) => p.id === card.priorityId)
  const resolution = resolutions.data?.find((r) => r.id === card.resolutionId)
  const cardType = cardTypes.data?.find((t) => t.id === card.typeId)

  const parent = card.parentId != null ? parentLookup?.get(card.parentId) : undefined

  return (
    <aside className={styles.sidebar} aria-label="Card details">
      <Field label="Status">
        <div className={styles.statusBlock}>
          {status ? (
            <Lozenge statusCategory={CATEGORY[status.category]} isBold>
              {status.name}
            </Lozenge>
          ) : (
            <Lozenge>Unknown</Lozenge>
          )}
          <TransitionButtons cardKey={card.key} />
        </div>
      </Field>

      {card.resolved && (
        <Field label="Resolution">
          <SelectValue
            value={card.resolutionId ?? ''}
            placeholder="Unresolved"
            options={(resolutions.data ?? []).map((r) => ({ value: r.id, label: r.name }))}
            display={resolution?.name}
            onChange={(value) => patch.mutate({ resolutionId: value === '' ? null : value })}
            label="Resolution"
          />
        </Field>
      )}

      <PeopleField
        label="Assignee"
        userId={card.assigneeId}
        projectKey={projectKey}
        onChange={(value) => patch.mutate({ assigneeId: value })}
        emptyLabel="Unassigned"
      />

      <PeopleField
        label="Reporter"
        userId={card.reporterId}
        projectKey={projectKey}
        onChange={(value) => patch.mutate({ reporterId: value })}
        emptyLabel="None"
      />

      <Field label="Priority">
        <SelectValue
          value={card.priorityId ?? ''}
          placeholder="None"
          options={(priorities.data ?? []).map((p) => ({ value: p.id, label: p.name }))}
          display={priority?.name}
          onChange={(value) => patch.mutate({ priorityId: value === '' ? null : value })}
          label="Priority"
        />
      </Field>

      <Field label="Type">
        <SelectValue
          value={card.typeId}
          options={(cardTypes.data ?? []).map((t) => ({ value: t.id, label: t.name }))}
          display={cardType?.name}
          onChange={(value) => patch.mutate({ typeId: value })}
          label="Type"
        />
      </Field>

      <TagField cardKey={card.key} projectKey={projectKey} />

      <Field label="Due date">
        <DateValue
          value={card.dueDate ?? ''}
          onChange={(value) => patch.mutate({ dueDate: value === '' ? null : value })}
          label="Due date"
        />
      </Field>

      <Field label="Start date">
        <DateValue
          value={card.startDate ?? ''}
          onChange={(value) => patch.mutate({ startDate: value === '' ? null : value })}
          label="Start date"
        />
      </Field>

      {card.parentId != null && (
        <Field label="Parent">
          {parent ? (
            <Link to="/cards/$key" params={{ key: parent.key }} className={styles.parentLink}>
              <span className={styles.parentKey}>{parent.key}</span>
              <span className={styles.parentSummary}>{parent.summary}</span>
            </Link>
          ) : (
            <span className={styles.fieldMuted}>In another card</span>
          )}
        </Field>
      )}

      <Development card={card} projectKey={projectKey} />

      <CardCycleField cardKey={card.key} projectKey={projectKey} />

      <div className={styles.timestamps}>
        <div title={formatDateTime(card.createdAt)}>Created {formatDate(card.createdAt)}</div>
        <div title={formatDateTime(card.updatedAt)}>Updated {formatDate(card.updatedAt)}</div>
        {card.resolved && card.resolvedAt && (
          <div title={formatDateTime(card.resolvedAt)}>Resolved {formatDate(card.resolvedAt)}</div>
        )}
      </div>
    </aside>
  )
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className={styles.field}>
      <span className={styles.fieldLabel}>{label}</span>
      <div className={styles.fieldValue}>{children}</div>
    </div>
  )
}

/** A person field. The members list is a cheap, shared, cached query reused by every field. */
function PeopleField({
  label,
  userId,
  projectKey,
  onChange,
  emptyLabel,
}: {
  label: string
  userId: string | null | undefined
  projectKey: string
  onChange: (value: string | null) => void
  emptyLabel: string
}) {
  const members = useMembers(projectKey).data
  const selected = members?.find((m) => m.userId === userId)

  return (
    <Field label={label}>
      <div className={styles.person}>
        {selected ? (
          <Avatar name={selected.displayName} size="small" />
        ) : (
          <span className={styles.personEmpty} aria-hidden="true" />
        )}
        <SelectValue
          value={userId ?? ''}
          placeholder={emptyLabel}
          options={(members ?? []).map((m) => ({ value: m.userId, label: m.displayName }))}
          display={selected?.displayName ?? emptyLabel}
          onChange={(value) => onChange(value === '' ? null : value)}
          label={label}
        />
      </div>
    </Field>
  )
}

interface Option {
  value: string
  label: string
}

/**
 * A native select styled to read as a value until focused — the sidebar's inline editor.
 *
 * The visible text is the selected label; the real `<select>` sits on top at zero opacity so
 * the whole row is a hit target with full native keyboard behaviour, and only the chevron
 * hints that it is editable. `onChange` fires the optimistic patch.
 */
function SelectValue({
  value,
  options,
  display,
  onChange,
  placeholder,
  label,
}: {
  value: string
  options: Option[]
  display?: string | undefined
  onChange: (value: string) => void
  placeholder?: string
  label: string
}) {
  const text = display ?? options.find((o) => o.value === value)?.label ?? placeholder ?? '—'
  const isEmpty = value === ''

  return (
    <label className={styles.inlineSelect}>
      <span className={isEmpty ? styles.fieldMuted : undefined}>{text}</span>
      <select
        className={styles.inlineSelectControl}
        value={value}
        aria-label={label}
        onChange={(event) => onChange(event.target.value)}
      >
        {placeholder !== undefined && <option value="">{placeholder}</option>}
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <span className={styles.inlineSelectChevron} aria-hidden="true">
        ⌄
      </span>
    </label>
  )
}

/** An inline date field: the formatted date until clicked, a native date picker to edit. */
function DateValue({
  value,
  onChange,
  label,
}: {
  value: string
  onChange: (value: string) => void
  label: string
}) {
  return (
    <label className={styles.inlineSelect}>
      <span className={value === '' ? styles.fieldMuted : undefined}>
        {value === '' ? 'None' : formatDate(value)}
      </span>
      <input
        type="date"
        className={styles.inlineSelectControl}
        value={value}
        aria-label={label}
        onChange={(event) => onChange(event.target.value)}
      />
      <span className={styles.inlineSelectChevron} aria-hidden="true">
        ⌄
      </span>
    </label>
  )
}

/** Tags, reusing the Phase 4 picker and its optimistic attach/detach. */
function TagField({ cardKey, projectKey }: { cardKey: string; projectKey: string }) {
  const projectTags = useProjectTags(projectKey)
  const cardTags = useCardTags(cardKey)
  const attach = useAttachTag(cardKey)
  const detach = useDetachTag(cardKey)
  const create = useCreateTag(projectKey)

  const selected: Tag[] = cardTags.data ?? []

  return (
    <div className={styles.field}>
      <TagPicker
        label="Tags"
        options={projectTags.data ?? []}
        selected={selected}
        onSelect={(tag) => attach.mutate(tag)}
        onDeselect={(tag) => detach.mutate(tag.id)}
        onCreate={(name) =>
          create.mutate(
            { name },
            {
              onSuccess: (tag) => attach.mutate(tag),
            },
          )
        }
        isCreating={create.isPending}
      />
    </div>
  )
}
