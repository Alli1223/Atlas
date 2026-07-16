import { Monitor, Moon, Sun } from 'lucide-react'

import { Button } from '@/components/ui'
import { ICON } from '@/lib/icon'
import { useTheme } from '@/providers/ThemeProvider'
import { type ThemePreference } from '@/stores/ui'

const NEXT: Record<ThemePreference, ThemePreference> = {
  light: 'dark',
  dark: 'system',
  system: 'light',
}

/** Glyph per preference — distinct from ICON, which is the shared Lucide sizing. */
const THEME_GLYPH = {
  light: Sun,
  dark: Moon,
  system: Monitor,
} as const

const LABEL: Record<ThemePreference, string> = {
  light: 'Light',
  dark: 'Dark',
  system: 'Match system',
}

/**
 * Cycles light -> dark -> system. A three-way toggle rather than a switch, because
 * "system" is a real third state and collapsing it loses the ability to follow the OS.
 */
export function ThemeToggle() {
  const { theme, setTheme } = useTheme()
  const Icon = THEME_GLYPH[theme]

  return (
    <Button
      appearance="subtle"
      isIconOnly
      onClick={() => setTheme(NEXT[theme])}
      aria-label={`Theme: ${LABEL[theme]}. Switch to ${LABEL[NEXT[theme]]}`}
      title={`Theme: ${LABEL[theme]}`}
      iconBefore={<Icon {...ICON} aria-hidden="true" />}
    />
  )
}
