import {
  Bookmark,
  Bug,
  CheckSquare,
  ChevronDown,
  ChevronsDown,
  ChevronsUp,
  ChevronUp,
  Circle,
  Equal,
  GitBranch,
  type LucideIcon,
  Minus,
  Sparkles,
  Square,
  Target,
  Zap,
} from 'lucide-react'

import type { ComponentProps } from 'react'

import type { CardType, Priority } from './api'

/**
 * The backend stores an icon *name* (a Lucide kebab-case id) and a hex colour on each card
 * type and priority — set by the project template. This resolves the name to a component.
 *
 * A closed map rather than a dynamic `lucide[name]` lookup: the set of icons a template can
 * seed is small and known, dynamic indexing defeats tree-shaking (it would pull the whole
 * icon set into the bundle), and an unknown name gets a sensible shape instead of a crash.
 */
const ICON_BY_NAME: Record<string, LucideIcon> = {
  target: Target,
  zap: Zap,
  bug: Bug,
  bookmark: Bookmark,
  'check-square': CheckSquare,
  'git-branch': GitBranch,
  sparkles: Sparkles,
  square: Square,
  'chevrons-up': ChevronsUp,
  'chevron-up': ChevronUp,
  equal: Equal,
  'chevron-down': ChevronDown,
  'chevrons-down': ChevronsDown,
  minus: Minus,
}

/** The card type's icon component, falling back to a filled square for an unknown name. */
export function cardTypeIcon(type: CardType | undefined): LucideIcon {
  if (!type?.icon) return Square
  return ICON_BY_NAME[type.icon] ?? Square
}

/** The priority's icon component, falling back to a neutral bar. */
export function priorityIcon(priority: Priority | undefined): LucideIcon {
  if (!priority?.icon) return Equal
  return ICON_BY_NAME[priority.icon] ?? Equal
}

/** A card type's brand colour, or a neutral fallback. Jira colours these fixed per theme. */
export function cardTypeColour(type: CardType | undefined): string {
  return type?.colour ?? 'var(--ds-icon-subtle)'
}

/** A priority's brand colour, or a neutral fallback. */
export function priorityColour(priority: Priority | undefined): string {
  return priority?.colour ?? 'var(--ds-icon-subtle)'
}

/** Re-exported so a fallback board card can render without a resolved type. */
export { Circle }

export interface GlyphProps extends ComponentProps<LucideIcon> {
  icon: LucideIcon
}

/**
 * Renders a resolved-at-runtime Lucide icon.
 *
 * A stable wrapper rather than `const Icon = resolve(); <Icon />` at the call site: assigning
 * a component to a local and rendering it trips the "component created during render" lint,
 * and a wrapper whose icon arrives as a *prop* is the idiomatic way to render a dynamic icon.
 */
export function Glyph({ icon: Icon, ...props }: GlyphProps) {
  return <Icon {...props} />
}
