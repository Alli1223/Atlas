import { api, unwrap } from '@/lib/api'
import type { components } from '@/lib/api-schema'

/** A tag. Mirrors `crate::domain::tag::Tag`. */
export type Tag = components['schemas']['Tag']

/** A tag plus its usage count in one project. Mirrors `crate::domain::tag::TagUsage`. */
export type TagUsage = components['schemas']['TagUsage']

/**
 * The palette a chip can be painted from. Mirrors `crate::domain::tag::TagColour`, and is
 * the same list as `TAG_COLORS` in `@/components/ui/Tag` — the backend's enum, the CSS
 * tokens and the primitive's prop type are three views of one set.
 */
export type TagColour = components['schemas']['TagColour']

export interface CreateTagInput {
  projectKey: string
  name: string
  colour?: TagColour
}

export interface UpdateTagInput {
  id: string
  name?: string
  /** `null` clears the colour back to the neutral chip. */
  colour?: TagColour | null
}

export interface AttachTagInput {
  cardKey: string
  tagId: string
}

export interface MergeTagInput {
  /** The tag to merge away. It will not exist afterwards. */
  id: string
  /** The tag that survives. */
  intoTagId: string
}

/**
 * Every tag a project offers — its own and every global one — with usage counts.
 *
 * `usageCount` is scoped to this project's live cards, so the same global tag reads a
 * different number in each project. That is the number the picker and the filter chips
 * want: "how many cards *here* have this".
 */
export async function fetchProjectTags(projectKey: string): Promise<TagUsage[]> {
  return unwrap(
    await api.GET('/api/v1/projects/{key}/tags', { params: { path: { key: projectKey } } }),
  )
}

/** The tags on one card. */
export async function fetchCardTags(cardKey: string): Promise<Tag[]> {
  return unwrap(await api.GET('/api/v1/cards/{key}/tags', { params: { path: { key: cardKey } } }))
}

/** Creates a tag. This is the picker's create-on-the-fly path. */
export async function createTag({ projectKey, name, colour }: CreateTagInput): Promise<Tag> {
  return unwrap(
    await api.POST('/api/v1/projects/{key}/tags', {
      params: { path: { key: projectKey } },
      body: { name, ...(colour !== undefined && { colour }) },
    }),
  )
}

/**
 * Renames and/or recolours a tag.
 *
 * A rename cannot orphan a card: `card_tags` references the tag's id, which does not
 * change. Every card carrying it keeps carrying it, under the new name.
 */
export async function updateTag({ id, name, colour }: UpdateTagInput): Promise<Tag> {
  return unwrap(
    await api.PATCH('/api/v1/tags/{id}', {
      params: { path: { id } },
      body: {
        ...(name !== undefined && { name }),
        // `null` is a meaningful value here (clear the colour) and `undefined` means
        // "leave it alone", so this cannot collapse into a `??`.
        ...(colour !== undefined && { colour }),
      },
    }),
  )
}

/** Deletes a tag, taking it off every card that carried it. */
export async function deleteTag(id: string): Promise<void> {
  unwrap(await api.DELETE('/api/v1/tags/{id}', { params: { path: { id } } }))
}

/**
 * Merges one tag into another.
 *
 * Every card carrying `id` ends up carrying `intoTagId`, and `id` stops existing. A card
 * that carried both is left with one chip, not two.
 */
export async function mergeTag({ id, intoTagId }: MergeTagInput): Promise<Tag> {
  const result = unwrap(
    await api.POST('/api/v1/tags/{id}/merge', {
      params: { path: { id } },
      body: { intoTagId },
    }),
  )
  return result.tag
}

/**
 * Puts a tag on a card, and returns the card's tags.
 *
 * Returns the whole list rather than the one attached, so the cache is replaced from one
 * authoritative answer instead of spliced. Idempotent on the backend: tagging a card that
 * already has the tag is a 200, not a 409.
 */
export async function attachTag({ cardKey, tagId }: AttachTagInput): Promise<Tag[]> {
  return unwrap(
    await api.POST('/api/v1/cards/{key}/tags', {
      params: { path: { key: cardKey } },
      body: { tagId },
    }),
  )
}

/** Takes a tag off a card. A 204 whether or not the card had it. */
export async function detachTag({ cardKey, tagId }: AttachTagInput): Promise<void> {
  unwrap(
    await api.DELETE('/api/v1/cards/{key}/tags/{tagId}', {
      params: { path: { key: cardKey, tagId } },
    }),
  )
}
