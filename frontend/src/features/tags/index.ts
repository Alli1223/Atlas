export { TagList } from './TagList'
export type { TagListProps } from './TagList'

export { TagPicker } from './TagPicker'
export type { TagPickerProps } from './TagPicker'

export {
  hyphenate,
  isValidTagName,
  MAX_TAG_NAME,
  rankTags,
  tagNameErrorMessage,
  validateTagName,
} from './name'
export type { TagNameError } from './name'

export {
  cardTagsQueryOptions,
  projectTagsQueryOptions,
  tagKeys,
  useAttachTag,
  useCardTags,
  useCreateTag,
  useDeleteTag,
  useDetachTag,
  useMergeTag,
  useProjectTags,
  useUpdateTag,
} from './queries'

export type { Tag, TagColour, TagUsage } from './api'
