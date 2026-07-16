import { createFileRoute } from '@tanstack/react-router'
import { ArrowRight, Inbox, Paperclip, Plus, Trash2 } from 'lucide-react'
import { type ReactNode, useState } from 'react'

import {
  Avatar,
  AvatarGroup,
  Banner,
  Button,
  type ButtonAppearance,
  Checkbox,
  EmptyState,
  Input,
  Lozenge,
  Radio,
  RadioGroup,
  Select,
  Skeleton,
  SkeletonText,
  Spinner,
  type SpinnerSize,
  Tag,
  TAG_COLORS,
  Textarea,
} from '@/components/ui'
import { ICON } from '@/lib/icon'

import styles from './styleguide.module.css'

export const Route = createFileRoute('/styleguide')({
  component: StyleguideRoute,
})

const RAMPS = [
  'blue',
  'red',
  'green',
  'lime',
  'yellow',
  'orange',
  'purple',
  'teal',
  'magenta',
] as const

const STOPS = [100, 200, 250, 300, 400, 500, 600, 700, 800, 850, 900, 1000] as const

const NEUTRAL_STOPS = [0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200] as const

const BUTTON_APPEARANCES: ButtonAppearance[] = [
  'primary',
  'default',
  'subtle',
  'link',
  'danger',
  'warning',
]

const SPACING = [
  '025', '050', '075', '100', '150', '200', '250', '300', '400', '500', '600', '800', '1000',
] as const

const TYPE_TOKENS = [
  'heading-xxlarge',
  'heading-xlarge',
  'heading-large',
  'heading-medium',
  'heading-small',
  'heading-xsmall',
  'heading-xxsmall',
  'body-large',
  'body',
  'body-small',
] as const

const SPINNER_SIZES: SpinnerSize[] = ['xsmall', 'small', 'medium', 'large']

function Section({
  title,
  note,
  children,
}: {
  title: string
  note?: string
  children: ReactNode
}) {
  return (
    <section className={styles.section}>
      <h3 className={styles.sectionTitle}>{title}</h3>
      {note !== undefined && <p className={styles.sectionNote}>{note}</p>}
      {children}
    </section>
  )
}

/** Every primitive, every variant. Rendered twice — once per theme. */
function Showcase() {
  const [isLoading, setLoading] = useState(false)

  return (
    <div className={styles.paneBody}>
      <Section
        title="Buttons"
        note="32px default / 24px compact. Warning is dark-on-yellow, never white-on-yellow."
      >
        <div className={styles.row}>
          {BUTTON_APPEARANCES.map((appearance) => (
            <Button key={appearance} appearance={appearance}>
              {appearance}
            </Button>
          ))}
        </div>
        <div className={styles.row}>
          {BUTTON_APPEARANCES.map((appearance) => (
            <Button key={appearance} appearance={appearance} size="compact">
              {appearance}
            </Button>
          ))}
        </div>
        <div className={styles.row}>
          {BUTTON_APPEARANCES.map((appearance) => (
            <Button key={appearance} appearance={appearance} disabled>
              {appearance}
            </Button>
          ))}
        </div>
        <div className={styles.row}>
          <Button appearance="primary" iconBefore={<Plus {...ICON} aria-hidden="true" />}>
            Icon before
          </Button>
          <Button iconAfter={<ArrowRight {...ICON} aria-hidden="true" />}>Icon after</Button>
          <Button
            appearance="subtle"
            isIconOnly
            aria-label="Attach file"
            iconBefore={<Paperclip {...ICON} aria-hidden="true" />}
          />
          <Button
            appearance="danger"
            isIconOnly
            aria-label="Delete"
            iconBefore={<Trash2 {...ICON} aria-hidden="true" />}
          />
          <Button appearance="primary" isLoading>
            Loading
          </Button>
          <Button
            onClick={() => {
              setLoading((v) => !v)
            }}
            isLoading={isLoading}
          >
            Toggle loading
          </Button>
        </div>
      </Section>

      <Section
        title="Lozenges"
        note="11px uppercase, weight 653. To Do = grey, In Progress = blue, Done = LIME (not green)."
      >
        <div className={styles.row}>
          <Lozenge statusCategory="todo">To Do</Lozenge>
          <Lozenge statusCategory="inprogress">In Progress</Lozenge>
          <Lozenge statusCategory="done">Done</Lozenge>
          <Lozenge appearance="removed">Removed</Lozenge>
          <Lozenge appearance="new">New</Lozenge>
          <Lozenge appearance="moved">Moved</Lozenge>
        </div>
        <div className={styles.row}>
          <Lozenge statusCategory="todo" isBold>
            To Do
          </Lozenge>
          <Lozenge statusCategory="inprogress" isBold>
            In Progress
          </Lozenge>
          <Lozenge statusCategory="done" isBold>
            Done
          </Lozenge>
          <Lozenge appearance="removed" isBold>
            Removed
          </Lozenge>
          <Lozenge appearance="new" isBold>
            New
          </Lozenge>
          <Lozenge appearance="moved" isBold>
            Moved
          </Lozenge>
        </div>
      </Section>

      <Section title="Tags" note="Chips for labels. Removable, linkable, rounded.">
        <div className={styles.row}>
          {TAG_COLORS.map((color) => (
            <Tag key={color} color={color}>
              {color}
            </Tag>
          ))}
        </div>
        <div className={styles.row}>
          <Tag color="blue" isRounded>
            rounded
          </Tag>
          <Tag color="green" href="#tag">
            linked
          </Tag>
          <Tag
            color="red"
            onRemove={() => {
              /* demo only */
            }}
          >
            removable
          </Tag>
          <Tag
            color="purple"
            isRounded
            onRemove={() => {
              /* demo only */
            }}
          >
            both
          </Tag>
        </div>
      </Section>

      <Section title="Avatars" note="16 / 24 / 32 / 40 / 96 / 128. Initials colour is stable per name.">
        <div className={styles.row}>
          <Avatar name="Alastair Rayner" size="xsmall" />
          <Avatar name="Alastair Rayner" size="small" />
          <Avatar name="Alastair Rayner" size="medium" />
          <Avatar name="Alastair Rayner" size="large" />
          <Avatar name="Grace Hopper" size="medium" appearance="square" />
          <AvatarGroup>
            <Avatar name="Ada Lovelace" size="small" isStacked />
            <Avatar name="Grace Hopper" size="small" isStacked />
            <Avatar name="Alan Turing" size="small" isStacked />
            <Avatar name="Katherine Johnson" size="small" isStacked />
          </AvatarGroup>
        </div>
      </Section>

      <Section title="Fields" note="32px controls, matching the 32px button they sit beside.">
        <div className={styles.fieldGrid}>
          <Input label="Summary" placeholder="What needs doing?" defaultValue="" />
          <Input label="Compact" size="compact" placeholder="Compact input" />
          <Input
            label="With help"
            helpMessage="Card keys are permanent; renaming a project keeps old links alive."
            defaultValue="ATLAS-42"
          />
          <Input label="Invalid" errorMessage="Summary is required" defaultValue="" />
          <Input label="Disabled" disabled defaultValue="Locked" />
          <Textarea label="Description" placeholder="Markdown supported" rows={3} />
          <Select
            label="Card type"
            placeholder="Choose a type"
            options={[
              { label: 'Story', value: 'story' },
              { label: 'Bug', value: 'bug' },
              { label: 'Task', value: 'task' },
              { label: 'Epic (disabled)', value: 'epic', isDisabled: true },
            ]}
          />
          <Select
            label="Invalid select"
            errorMessage="Pick one"
            options={[{ label: 'Story', value: 'story' }]}
          />
        </div>
      </Section>

      <Section title="Choices">
        <div className={styles.row}>
          <Checkbox label="Unchecked" />
          <Checkbox label="Checked" defaultChecked />
          <Checkbox label="Indeterminate" isIndeterminate />
          <Checkbox label="Disabled" disabled />
          <Checkbox label="Checked + disabled" defaultChecked disabled />
          <Checkbox label="Invalid" isInvalid />
        </div>
        <div className={styles.row}>
          <RadioGroup
            label="Estimation"
            name="styleguide-estimation"
            defaultValue="points"
            options={[
              { label: 'Story points', value: 'points' },
              { label: 'Hours', value: 'hours' },
              { label: 'None', value: 'none' },
              { label: 'Unavailable', value: 'x', isDisabled: true },
            ]}
          />
          <Radio name="styleguide-standalone" label="Standalone" />
        </div>
      </Section>

      <Section title="Spinners">
        <div className={styles.row}>
          {SPINNER_SIZES.map((size) => (
            <Spinner key={size} size={size} />
          ))}
        </div>
      </Section>

      <Section title="Banners" note="Errors use role=alert; announcements use role=status.">
        <div className={styles.stack}>
          <Banner>Atlas is running in development mode.</Banner>
          <Banner appearance="warning" actions={<Button size="compact">Renew</Button>}>
            Your GitHub token expires in 3 days.
          </Banner>
          <Banner appearance="error" actions={<Button size="compact">Retry</Button>}>
            The Claude Code session failed to start.
          </Banner>
        </div>
      </Section>

      <Section title="Skeletons">
        <div className={styles.stack}>
          <div className={styles.row}>
            <Skeleton width={32} height={32} isCircle />
            <Skeleton width={160} height={12} />
            <Skeleton width={64} height={16} hasShimmer={false} />
          </div>
          <SkeletonText lines={3} />
        </div>
      </Section>

      <Section title="Empty state">
        <EmptyState
          isCompact
          image={<Inbox size={40} strokeWidth={1.5} strokeLinecap="square" />}
          header="No cards match this filter"
          description="Try removing a quick filter, or create the first card in this column."
          primaryAction={<Button appearance="primary">Create card</Button>}
          secondaryAction={<Button appearance="subtle">Clear filters</Button>}
        />
      </Section>

      <Section
        title="Board card"
        note="Composed from the primitives: 250px column, 8px gutter, raised card on a sunken column."
      >
        <div className={styles.boardColumn}>
          <div className={styles.boardColumnHeader}>
            <span>In Progress</span>
            <span>2</span>
          </div>
          <div className={styles.card}>
            <span className={styles.cardTitle}>Wire the board to the AQL query engine</span>
            <div className={styles.row}>
              <Tag color="blue">frontend</Tag>
              <Tag color="red">blocked</Tag>
            </div>
            <div className={styles.cardFooter}>
              <span className={styles.cardKey}>ATLAS-42</span>
              <div className={styles.cardMeta}>
                <Lozenge statusCategory="inprogress">In Progress</Lozenge>
                <Avatar name="Alastair Rayner" size="xsmall" />
              </div>
            </div>
          </div>
          <div className={styles.card}>
            <span className={styles.cardTitle}>Retopologise the hero asset</span>
            <div className={styles.row}>
              <Tag color="purple">retopo</Tag>
            </div>
            <div className={styles.cardFooter}>
              <span className={styles.cardKey}>ATLAS-43</span>
              <div className={styles.cardMeta}>
                <Lozenge statusCategory="done">Done</Lozenge>
                <Avatar name="Grace Hopper" size="xsmall" />
              </div>
            </div>
          </div>
        </div>
      </Section>

      <Section title="Elevation" note="Dark mode lifts surfaces by getting lighter, not by shadow.">
        <div className={styles.elevationGrid}>
          <div className={`${styles.elevationTile} ${styles.sunken}`}>sunken</div>
          <div className={`${styles.elevationTile} ${styles.raised}`}>raised</div>
          <div className={`${styles.elevationTile} ${styles.overlay}`}>overlay</div>
        </div>
      </Section>

      <Section title="Type scale" note="Body is 14px/20px. Headings are weight 653 — a real Inter axis.">
        <div>
          {TYPE_TOKENS.map((token) => (
            <div key={token} className={styles.typeRow}>
              <span className={styles.typeToken}>{token}</span>
              <span style={{ font: `var(--ds-font-${token})` }}>Atlas</span>
            </div>
          ))}
        </div>
      </Section>

      <Section title="Spacing" note="space.100 (8px) is the workhorse gutter, not 16px.">
        <div>
          {SPACING.map((step) => (
            <div key={step} className={styles.spaceRow}>
              <span className={styles.spaceToken}>space.{step}</span>
              <span className={styles.spaceBar} style={{ width: `var(--ds-space-${step})` }} />
            </div>
          ))}
        </div>
      </Section>

      <Section title="Colour ramps">
        <div className={styles.stack}>
          {RAMPS.map((ramp) => (
            <div key={ramp} className={styles.ramp}>
              <span className={styles.rampLabel}>{ramp}</span>
              <div className={styles.rampRow}>
                {STOPS.map((stop) => (
                  <span
                    key={stop}
                    className={`${styles.swatch} ${stop <= 300 ? styles.swatchDarkText : ''}`}
                    style={{ background: `var(--ads-${ramp}-${stop})` }}
                    title={`--ads-${ramp}-${stop}`}
                  >
                    {stop}
                  </span>
                ))}
              </div>
            </div>
          ))}
          <div className={styles.ramp}>
            <span className={styles.rampLabel}>neutral</span>
            <div className={styles.rampRow}>
              {NEUTRAL_STOPS.map((stop) => (
                <span
                  key={stop}
                  className={`${styles.swatch} ${stop <= 300 ? styles.swatchDarkText : ''}`}
                  style={{ background: `var(--ads-n-${stop})` }}
                  title={`--ads-n-${stop}`}
                >
                  {stop}
                </span>
              ))}
            </div>
          </div>
        </div>
      </Section>
    </div>
  )
}

function StyleguideRoute() {
  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h1>Style guide</h1>
        <p className={styles.lede}>
          Every primitive, every variant, in both themes at once — so a token regression is
          visible rather than reported.
        </p>
        <p className={styles.note}>
          Values come from @atlaskit/tokens@15.8.0 (brand-refresh palette). The two panes below
          are theme islands: they set data-theme on themselves and ignore your current theme, so
          you can check both without toggling.
        </p>
      </header>

      <div className={styles.panes}>
        <section className={styles.pane} data-theme="light">
          <header className={styles.paneHeader}>
            <span>Light</span>
            <Lozenge statusCategory="done">data-theme=&quot;light&quot;</Lozenge>
          </header>
          <Showcase />
        </section>

        <section className={styles.pane} data-theme="dark">
          <header className={styles.paneHeader}>
            <span>Dark</span>
            <Lozenge statusCategory="done">data-theme=&quot;dark&quot;</Lozenge>
          </header>
          <Showcase />
        </section>
      </div>
    </div>
  )
}
