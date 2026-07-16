# React 19 / TypeScript / Vite frontend stack for Atlas (Jira-like board app on a Rust/Axum backend) — 2026 best practice, version-verified against npm and crates.io on 2026-07-16

> Researched 2026-07-16 for the Atlas build. Claims marked `uncertain`/`likely` were put
> through an adversarial verification pass; see `corrections.md` for what was refuted.

## Summary

The 2026 baseline moved significantly: Vite 8.1.5 replaced esbuild+Rollup with Rolldown, @vitejs/plugin-react 6 dropped built-in Babel (React Compiler now wires through @rolldown/plugin-babel), React Compiler hit 1.0.0 stable, and react-router 8.2.0 deleted the react-router-dom package entirely. The single most decisive finding is on drag-and-drop: react-beautiful-dnd is formally deprecated and peer-caps at React 18 (disqualified), @dnd-kit/core 6.3.1 has not published since 2024-12-05 (19 months stale) with its successor @dnd-kit/react still at 0.5.0, while @atlaskit/pragmatic-drag-and-drop 2.0.1 is the library Atlassian actually ships Jira/Trello/Confluence on, is 4.7kB, has zero React peer dependency, and had a sub-package published *today*. Critically, I disproved a widely-repeated blog claim: PDND does **not** support keyboard dragging (space-to-lift/arrow-to-move) — Atlassian's documented position is that action menus ("Move to column…") beat directional keyboard DnD in user testing, and that is precisely why Jira works the way it does, so this is the authentic pattern, not a gap. Second major trap: TypeScript 7.0.2 (the Go port, stable 2026-07-08) is `latest` on npm, but typescript-eslint 8.64.0 peer-caps at `typescript >=4.8.4 <6.1.0` with no v9 RC published — installing TS 7 silently breaks type-aware linting, so Atlas should pin TypeScript 6.0.3.

## Implementation notes

RECOMMENDATION SUMMARY (each justified below):
- Vite 8.1.5 + @vitejs/plugin-react 6.0.3 + React 19.2.7 + React Compiler 1.0.0
- **TypeScript 6.0.3, NOT 7.0.2** (see risk #1 — this is the highest-value finding in this report)
- **TanStack Router 1.170.18** over react-router 8.2.0
- TanStack Query 5.101.2
- **@atlaskit/pragmatic-drag-and-drop 2.0.1** (Atlassian's own; powers Jira)
- zustand 5.0.14 · react-hook-form 7.81.0 + zod 4.4.3 + @hookform/resolvers 5.4.0
- @tanstack/react-virtual 3.14.6 · Tiptap 3.28.0 · @xterm/xterm 6.0.0 · recharts 3.9.2
- vitest 4.1.10 + @testing-library/react 16.3.2 + playwright 1.61.1
- openapi-typescript 7.13.0 + openapi-fetch 0.17.0, fed by utoipa 5.5.0 on the Axum side

═══════════════════════════════════════
1. VITE 8 CONFIG (proxy to Axum + env + React Compiler)
═══════════════════════════════════════
Scaffold: `npm create vite@latest atlas-web -- --template react-ts` (create-vite 9.1.1). Skip react-swc-ts — on Vite 8, Rolldown/Oxc already does the transform natively, so the SWC template's reason for existing is largely gone.

Note the Vite 8 gotcha: @vitejs/plugin-react v6 **dropped built-in Babel**, so the React Compiler is no longer a plugin option — it goes through @rolldown/plugin-babel, and `babel()` MUST come before `react()`.

```ts
// vite.config.ts
import { defineConfig } from 'vite'
import react, { reactCompilerPreset } from '@vitejs/plugin-react'
import { babel } from '@rolldown/plugin-babel'
import { tanstackRouter } from '@tanstack/router-plugin/vite'

export default defineConfig({
  plugins: [
    tanstackRouter({ target: 'react', autoCodeSplitting: true }),
    babel({ presets: [reactCompilerPreset()] }), // MUST precede react()
    react(),
  ],
  server: {
    port: 5173,
    proxy: {
      // Axum backend
      '/api': { target: 'http://127.0.0.1:8080', changeOrigin: true },
      // live board sync + Claude Code output stream
      '/ws': { target: 'ws://127.0.0.1:8080', ws: true },
    },
  },
})
```
Keep the `/api` prefix identical on both sides so no `rewrite` is needed — it removes a whole class of "works in dev, 404s in prod" bugs. In production the Axum binary serves the built assets and the proxy is irrelevant, which is the right shape for a single-static-binary deploy (matches CLAUDE.md).

Do NOT set `rewriteWsOrigin: true` unless you must — Vite's own docs flag it as a CSRF footgun.

Env vars — only `VITE_`-prefixed values reach the client (envPrefix default). Load order: `.env` → `.env.local` → `.env.[mode]` → `.env.[mode].local`.
```ts
// src/vite-env.d.ts
/// <reference types="vite/client" />
interface ImportMetaEnv {
  readonly VITE_API_URL: string
  readonly VITE_WS_URL: string
}
interface ImportMeta { readonly env: ImportMetaEnv }
```
Atlas-specific: encrypted PATs/API keys are backend-only. There is no `VITE_` variable that should ever hold a secret — anything with the VITE_ prefix is inlined into the bundle in plaintext. That is a direct hard-fail against the CLAUDE.md non-negotiable on secrets; consider a lint/CI grep for `VITE_.*(TOKEN|KEY|SECRET|PAT)`.

═══════════════════════════════════════
2. ROUTING — recommend TanStack Router 1.170.18
═══════════════════════════════════════
react-router 8.2.0 is a fine library, but for Atlas specifically TanStack Router wins on three points that map directly onto your domain:

(a) **Recursive boards.** A card may contain a board, so your URL is arbitrarily deep: `/board/$boardId/card/$cardId` where the card opens another board. TanStack Router's typed route tree + `autoCodeSplitting` handles the recursion with inferred param types at every level; react-router gives you `string | undefined` params you must re-validate by hand at each nesting depth.

(b) **URL-driven state is the whole point.** Board filters (assignee, tag, project type), the open card modal, and swimlane grouping all belong in the URL. TanStack Router has first-class *validated* search params with Zod schemas, defaults, and structural sharing — meaning a component reading `?filter.tags` only re-renders when that slice changes. React Router has no equivalent; you'd hand-roll `useSearchParams` + parsing + memoisation, and that hand-rolled layer is exactly where filter bugs live.

(c) **No SSR requirement.** react-router v8's main pull is its framework/Remix-style story (middleware, route modules, SSR). Atlas is a self-hosted SPA served by an Axum binary — you'd pay v8's complexity and its Node 22.22 floor for capabilities you never use.

Also weigh: react-router 8 requires `react >= 19.2.7` exactly, is ESM-only, and **deleted `react-router-dom`** — so every migration guide, StackOverflow answer, and LLM-generated snippet you find that imports from `react-router-dom` is now wrong. Starting greenfield on TanStack Router sidesteps that entire confusion.

```ts
// src/routes/board.$boardId.tsx
import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'

const boardSearchSchema = z.object({
  card: z.string().optional(),            // open card modal — URL-driven
  tags: z.array(z.string()).default([]),
  assignee: z.string().optional(),
  group: z.enum(['none', 'assignee', 'tag']).default('none'),
})

export const Route = createFileRoute('/board/$boardId')({
  validateSearch: boardSearchSchema,
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(boardQuery(params.boardId)),
  component: BoardView,
})

function BoardView() {
  const { boardId } = Route.useParams()          // typed string
  const { card, tags, group } = Route.useSearch() // fully typed, validated
  const navigate = Route.useNavigate()
  // opening a card is a navigation, not a useState — survives refresh + is linkable
  const openCard = (id: string) =>
    navigate({ search: (prev) => ({ ...prev, card: id }) })
}
```
Wire the QueryClient into router context so loaders and components share one cache:
```ts
const router = createRouter({ routeTree, context: { queryClient } })
declare module '@tanstack/react-router' {
  interface Register { router: typeof router }
}
```

═══════════════════════════════════════
3. OPTIMISTIC CARD MOVE (the critical path — CLAUDE.md: "must feel instant")
═══════════════════════════════════════
The four rules that make this correct rather than merely fast: **cancelQueries first** (else an in-flight GET resolves after your optimistic write and visibly snaps the card back), **snapshot before mutate**, **rollback in onError from context**, **invalidate in onSettled not onSuccess** (onSuccess skips the rollback path, leaving the client authoritative after a failure — a silent desync).

```ts
// src/features/board/useMoveCard.ts
type MoveCard = { cardId: string; toColumnId: string; toIndex: number }

export function useMoveCard(boardId: string) {
  const qc = useQueryClient()
  const key = boardKeys.detail(boardId)

  return useMutation({
    mutationFn: (m: MoveCard) =>
      api.PATCH('/api/cards/{id}/move', { params: { path: { id: m.cardId } }, body: m }),

    onMutate: async (move) => {
      // 1. stop in-flight refetches clobbering the optimistic write
      await qc.cancelQueries({ queryKey: key })
      // 2. snapshot for rollback
      const previous = qc.getQueryData<Board>(key)
      // 3. apply optimistically — synchronous, so the card lands under the cursor
      qc.setQueryData<Board>(key, (old) => (old ? applyMove(old, move) : old))
      return { previous }
    },

    onError: (_err, _move, ctx) => {
      if (ctx?.previous) qc.setQueryData(key, ctx.previous) // 4. roll back
      toast.error('Could not move card — put it back')
    },

    // 5. onSettled, NOT onSuccess: also re-syncs after a rollback
    onSettled: () => { qc.invalidateQueries({ queryKey: key }) },
  })
}
```
`applyMove` must be a pure function (use immer 11.1.15 `produce`) so it is unit-testable in isolation and reusable by the WebSocket handler in §7:
```ts
const applyMove = (board: Board, m: MoveCard): Board => produce(board, (d) => {
  const from = d.columns.find(c => c.cards.some(x => x.id === m.cardId))!
  const i = from.cards.findIndex(x => x.id === m.cardId)
  const [card] = from.cards.splice(i, 1)
  d.columns.find(c => c.id === m.toColumnId)!.cards.splice(m.toIndex, 0, card)
})
```
Rapid-fire drags: set `networkMode` and consider a mutation scope so concurrent moves on one board serialise rather than interleave — otherwise two fast drags can invalidate each other's snapshots:
```ts
scope: { id: `board-${boardId}` },  // serialises mutations in this scope
```
Backend contract that makes this robust: have Axum accept a **fractional/lexicographic rank** (e.g. LexoRank-style string, as Jira literally does) rather than an integer index. Integer indices force a full-column renumber per move, produce write amplification in SQLite, and make concurrent moves conflict. A rank string means a move is a single-row UPDATE and two clients moving different cards never collide.

═══════════════════════════════════════
4. DRAG AND DROP — recommend @atlaskit/pragmatic-drag-and-drop 2.0.1
═══════════════════════════════════════
The field, honestly assessed:
| Library | Version | Status |
|---|---|---|
| react-beautiful-dnd | 13.1.1 | **Deprecated on npm**; peer caps at React 18 — literally cannot install. Dead. |
| @hello-pangea/dnd | 18.0.1 | Maintained rbd fork, React 19 OK, has real keyboard DnD — but 17 months stale and inherits rbd's perf ceiling. |
| @dnd-kit/core | 6.3.1 | **Last published 2024-12-05 — 19 months stale.** |
| @dnd-kit/react | 0.5.0 | Active (2026-07-13) but pre-1.0; unstable API. |
| **@atlaskit/pragmatic-drag-and-drop** | **2.0.1** | **4.7kB core, published 2026-06-17, sub-package published today. Powers Jira, Trello, Confluence.** |

You asked me to investigate PDND seriously; it is the right call, and the reasoning is not just "Atlassian made it":
- **Authenticity is literal.** Jira's board feel *is* PDND's feel. You cannot get closer.
- **Zero React coupling.** Core has no React peer dep at all (deps: bind-event-listener, raf-schd). It won't fight React 19 or the Compiler, and it can't be broken by a React major.
- **Performance at board scale.** It defers to native browser DnD rather than tracking pointer moves in React state — no re-render per mousemove, which is exactly the failure mode rbd/dnd-kit hit on big boards.
- **Maintenance is not a question.** Jira/Trello/Confluence depend on it, so it cannot be abandoned without Atlassian breaking itself. dnd-kit's stable core, by contrast, has shipped nothing in 19 months.
- **Auto-scroll is a first-class package** (-auto-scroll@3.0.0) with the over-element/near-edge behaviour tuned by the team that shipped it to millions of Jira users. This is the single hardest thing to get right by hand on a multi-column board and it is the usual reason DIY boards feel wrong.

**The honest tradeoff — read this before committing.** PDND does **not** do keyboard dragging. Blog posts claim @atlaskit/pragmatic-drag-and-drop-react-accessibility gives you Space-to-lift/arrows/Enter-to-drop; I checked Atlassian's own accessibility guidelines and **that is false**. PDND builds on native HTML5 DnD, and no browser implements the HTML5 spec's keyboard model. Atlassian's documented position, from their user testing: action menus beat directional keyboard DnD — cheaper to build, more discoverable, more reliable under screen readers. Their guidance: *"Every draggable item should have an accessible way to achieve the same outcome without using drag and drop."*

So if your a11y bar is "keyboard users can reorder cards", PDND meets it — via a **"Move to…" action menu**, not via arrow keys. That is what Jira actually does, so it is the Jira-authentic answer, but it is work you must do rather than a sensor you switch on. If you specifically want arrow-key dragging out of the box and will accept a stale dependency, @hello-pangea/dnd 18.0.1 is the only live option. My recommendation is PDND + action menu; budget for the menu explicitly.

```ts
// Card — draggable + drop target with edge detection
import { draggable, dropTargetForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'
import { combine } from '@atlaskit/pragmatic-drag-and-drop/combine'
import { attachClosestEdge, extractClosestEdge } from '@atlaskit/pragmatic-drag-and-drop-hitbox/closest-edge'

function Card({ card, columnId }: { card: Card; columnId: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [state, setState] = useState<'idle' | 'dragging' | 'over'>('idle')
  const [edge, setEdge] = useState<Edge | null>(null)

  useEffect(() => {
    const el = ref.current!
    return combine(
      draggable({
        element: el,
        getInitialData: () => ({ type: 'card', cardId: card.id, columnId }),
        onDragStart: () => setState('dragging'),
        onDrop: () => setState('idle'),
      }),
      dropTargetForElements({
        element: el,
        canDrop: ({ source }) => source.data.type === 'card',
        getData: ({ input }) =>
          attachClosestEdge({ type: 'card', cardId: card.id, columnId },
            { element: el, input, allowedEdges: ['top', 'bottom'] }),
        onDrag: ({ self }) => setEdge(extractClosestEdge(self.data)),
        onDragLeave: () => setEdge(null),
        onDrop: () => setEdge(null),
      }),
    )
  }, [card.id, columnId])

  return (
    <div ref={ref} style={{ opacity: state === 'dragging' ? 0.4 : 1 }}>
      {card.title}
      {edge && <DropIndicator edge={edge} gap="8px" />}
    </div>
  )
}
```
Column: scrollable drop target + auto-scroll (the bit that sells the feel):
```ts
import { autoScrollForElements } from '@atlaskit/pragmatic-drag-and-drop-auto-scroll/element'

useEffect(() => combine(
  dropTargetForElements({ element: listRef.current!, getData: () => ({ type: 'column', columnId }) }),
  autoScrollForElements({ element: listRef.current!, canScroll: ({ source }) => source.data.type === 'card' }),
), [columnId])
```
Single global monitor turns a drop into the mutation from §3 — one place, not per-card:
```ts
import { monitorForElements } from '@atlaskit/pragmatic-drag-and-drop/element/adapter'

useEffect(() => monitorForElements({
  canMonitor: ({ source }) => source.data.type === 'card',
  onDrop: ({ location, source }) => {
    const target = location.current.dropTargets[0]
    if (!target) return
    moveCard.mutate(resolveMove(source.data, target.data, extractClosestEdge(target.data)))
  },
}), [moveCard])
```
Add `-live-region@2.0.0` to announce "Card moved to In Progress, position 2" to screen readers, and `-flourish@3.0.1` for the drop triumph animation if you want the full Trello polish. For the recursive-board minimap, PDND's framework-agnostic core means a card that *contains* a board can be a drop target for cards at the parent level without any nested-context gymnastics — a genuine structural win over rbd-style libraries here.

═══════════════════════════════════════
5. CLIENT STATE — recommend zustand 5.0.14
═══════════════════════════════════════
First, scope it down: with TanStack Query owning server state and TanStack Router owning URL state (filters, open card, grouping), what's actually left is small — theme, modal stack, command palette, sidebar collapse, drag ghost preview. Don't over-provision for it.

- **Context**: fine for theme alone, but every consumer re-renders on any change. Wrong for a board with hundreds of nodes.
- **jotai 2.20.2**: excellent when state is genuinely atomic/derived-heavy. Atlas's UI state isn't; you'd get atom sprawl for little benefit.
- **zustand 5.0.14**: one store, selector-based subscriptions (only components reading `theme` re-render when theme changes), usable outside React (`store.getState()` from the WS handler or a PDND monitor callback — genuinely useful here), ~1kB, no provider.

```ts
// src/stores/ui.ts
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

type UIState = {
  theme: 'light' | 'dark' | 'system'
  openModal: { type: 'card'; id: string } | { type: 'settings' } | null
  sidebarCollapsed: boolean
  setTheme: (t: UIState['theme']) => void
  openCardModal: (id: string) => void
  closeModal: () => void
}

export const useUI = create<UIState>((set) => ({
  theme: 'system',
  openModal: null,
  sidebarCollapsed: false,
  setTheme: (theme) => set({ theme }),
  openCardModal: (id) => set({ openModal: { type: 'card', id } }),
  closeModal: () => set({ openModal: null }),
}))

// selector — this component re-renders ONLY when theme changes
const theme = useUI((s) => s.theme)
// multiple slices without over-rendering (zustand v5 requires useShallow explicitly)
const { theme, sidebarCollapsed } = useUI(useShallow((s) => ({ theme: s.theme, sidebarCollapsed: s.sidebarCollapsed })))
```
zustand v5 gotcha: v4's `create(...)` auto-shallow behaviour is gone — object-returning selectors **must** use `useShallow` or you get infinite re-renders. This bites people migrating.

Caveat worth stating: the card modal is in the URL (§2), so `openModal` should probably only cover non-linkable overlays (settings, command palette). Don't duplicate card-open state in both places — pick the URL.

═══════════════════════════════════════
6. FORMS — react-hook-form 7.81.0 + zod 4.4.3 + @hookform/resolvers 5.4.0
═══════════════════════════════════════
Version-pairing matters: Zod 4 is a rewrite, and **resolvers v5** is the release that supports it. resolvers v3 + zod 4 fails confusingly — a common 2026 install error.
```ts
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const cardSchema = z.object({
  title: z.string().min(1, 'Title is required').max(255),
  description: z.string().optional(),
  tags: z.array(z.string()).default([]),
  githubUrl: z.url().optional().or(z.literal('')), // zod 4: z.url(), not z.string().url()
})
type CardForm = z.infer<typeof cardSchema>

const { register, handleSubmit, formState: { errors, isSubmitting } } =
  useForm<CardForm>({ resolver: zodResolver(cardSchema), defaultValues: { tags: [] } })
```
Zod 4 API changes to know: `z.string().url()` → `z.url()`, `.email()` → `z.email()`; error customisation moved to a unified `error` param. Reuse these schemas for the OpenAPI-adjacent runtime validation in §13 so the board's "project type determines card template" logic has one source of truth per project type.

═══════════════════════════════════════
7. WEBSOCKET → QUERY CACHE (live board sync)
═══════════════════════════════════════
Two strategies; use both, chosen per event type. **setQueryData** for high-frequency, self-describing events (a card moved — you have the full delta, applying it is instant and refetch-free). **invalidateQueries** for coarse/ambiguous events (board settings changed, columns restructured) where refetching is simpler and safer than replaying a delta.

Reuse the *same* pure `applyMove` from §3 — this is the payoff of keeping it pure: a local optimistic move and a remote pushed move go through identical logic, so they cannot drift.

```ts
// src/features/sync/useBoardSocket.ts
type ServerEvent =
  | { kind: 'card.moved'; boardId: string; move: MoveCard; actorId: string }
  | { kind: 'card.updated'; boardId: string; card: Card }
  | { kind: 'board.restructured'; boardId: string }
  | { kind: 'agent.output'; cardId: string; chunk: string }

export function useBoardSocket(boardId: string) {
  const qc = useQueryClient()
  useEffect(() => {
    const ws = new WebSocket(`${import.meta.env.VITE_WS_URL}/ws/board/${boardId}`)
    ws.onmessage = (e) => {
      const ev: ServerEvent = JSON.parse(e.data)
      const key = boardKeys.detail(ev.boardId ?? boardId)
      switch (ev.kind) {
        case 'card.moved':
          // ignore our own echo — we already applied it optimistically
          if (ev.actorId === myClientId) return
          qc.setQueryData<Board>(key, (old) => (old ? applyMove(old, ev.move) : old))
          break
        case 'card.updated':
          qc.setQueryData<Board>(key, (old) => old && upsertCard(old, ev.card))
          break
        case 'board.restructured':
          qc.invalidateQueries({ queryKey: key }) // coarse → let Query refetch
          break
      }
    }
    return () => ws.close()
  }, [boardId, qc])
}
```
Three things that will bite you if unhandled:
- **Echo suppression.** Send a per-client `actorId`/connection id; ignore events you caused. Without it your own optimistic move gets re-applied and cards visibly double-hop.
- **Optimistic-vs-push race.** If a push lands mid-mutation it can overwrite an in-flight optimistic write. Guard with `qc.isMutating({ mutationKey })` and prefer invalidation once the mutation settles.
- **Reconnect = resync.** On WS reopen, `invalidateQueries` the board — you have no idea what you missed while disconnected. Pair with a monotonic server sequence number to detect gaps.

═══════════════════════════════════════
8. VIRTUALISATION — @tanstack/react-virtual 3.14.6
═══════════════════════════════════════
Headless, works with the PDND column scroll container. Threshold guidance: don't virtualise a board column (rarely >50 cards, and virtualisation actively fights drag auto-scroll and drop-target registration since offscreen targets don't exist). **Do** virtualise the backlog list and the agent log view.
```ts
const virtualizer = useVirtualizer({
  count: cards.length,
  getScrollElement: () => scrollRef.current,
  estimateSize: () => 92,
  overscan: 8, // raise if drag-scrolling reveals blanks
  measureElement: (el) => el.getBoundingClientRect().height, // variable card heights
})
```
If you ever must combine virtualisation with DnD, keep `overscan` generous and accept that drop targets outside the window won't register — the action-menu fallback from §4 covers those cases, which is another argument for building it.

═══════════════════════════════════════
9. RICH TEXT — recommend Tiptap 3.28.0
═══════════════════════════════════════
Jira's ADF is ProseMirror-based. Tiptap *is* ProseMirror (@tiptap/pm 3.28.0) with a React layer — so you get Jira-shaped document semantics, and if you ever want ADF-compatible JSON you're already in the right document model. Lexical 0.48.0 is still 0.x after years and is Meta-internally-driven; Plate 53.2.4 is Slate-based (different model, weaker structured-doc story) and just went through a `@udecode/plate` → `platejs` rename that stranded a lot of docs.

All five of your requirements are MIT/free — no Pro licence needed for Atlas's self-hosted scope:
```
@tiptap/react @tiptap/starter-kit @tiptap/pm              # 3.28.0 — core, MIT
@tiptap/extension-mention                                  # @mentions
@tiptap/extension-code-block-lowlight + lowlight@3.3.0     # code blocks w/ highlighting
StarterKit includes TaskList/TaskItem (checklists), Image, and markdown-ish input rules
```
```ts
const editor = useEditor({
  extensions: [
    StarterKit.configure({ codeBlock: false }),
    CodeBlockLowlight.configure({ lowlight: createLowlight(common) }),
    Mention.configure({
      HTMLAttributes: { class: 'mention' },
      suggestion: {
        char: '@',
        items: ({ query }) => searchMembers(query),   // hit your Axum endpoint
        render: () => renderMentionDropdown(),
      },
    }),
    TaskList, TaskItem.configure({ nested: true }),
    Image.configure({ allowBase64: false }),           // upload to Axum, store URL
  ],
  content: card.descriptionJson,
  onUpdate: ({ editor }) => debouncedSave(editor.getJSON()),
})
```
Store `editor.getJSON()` (ProseMirror doc JSON) in SQLite, not HTML — it survives schema evolution, is diffable, and keeps the ADF door open. Markdown paste: Tiptap's input rules cover typed markdown; for *pasted* markdown blocks, `tiptap-markdown` (0.9.0) exists but is third-party and I'd treat its maintenance as uncertain — a custom paste handler running the clipboard text through your own markdown parser is more predictable.

Licensing caveat you should know before building: Comments/Snapshots/AI are Pro and require the paid Cloud platform ($49/mo+, free tier removed June 2025). If Atlas wants card comments, build them as your own Axum-backed entities with a plain Tiptap editor per comment — do NOT reach for Tiptap's Comments extension, which would drag a cloud dependency into a self-hosted app.

═══════════════════════════════════════
10. TERMINAL / CLAUDE CODE OUTPUT — recommend @xterm/xterm 6.0.0
═══════════════════════════════════════
Use the **@xterm scope** — unscoped `xterm` is frozen at 5.3.0 and abandoned; @xterm/xterm 6.0.0 was published 2026-07-15.

Recommendation depends on one question: **is the Claude Code session interactive?** Per CLAUDE.md the agent runner is a `claude` CLI subprocess behind a trait.
- If you ever want to *attach* — send input, handle prompts, resize a PTY, let it render a TUI/spinner/progress bar — **xterm.js is the only real option.** It's a real terminal emulator: full ANSI/VT sequence support including cursor addressing, in-place rewrites, and carriage-return progress bars. A virtualised log view renders those as garbage, because `\r`-overwrite and cursor-up sequences are not colour codes and can't be regex'd away.
- If output is strictly append-only, one-way, colour-only, and searchable-as-text, a virtualised log view (TanStack Virtual + `anser` 2.3.5 for ANSI→spans) is lighter and gives you DOM-native find/select/copy.

Given Atlas runs the real `claude` CLI — which emits spinners and redraws — go with **xterm.js**. Add `@xterm/addon-fit` 0.11.0 (resize), `@xterm/addon-webgl` 0.19.0 (GPU rendering; essential for fast streams), `@xterm/addon-search` 0.16.0, `@xterm/addon-serialize` 0.14.0 (persist scrollback into SQLite so a card's session survives reload).
```ts
const term = new Terminal({ fontFamily: 'JetBrains Mono, monospace', fontSize: 13, scrollback: 10_000, convertEol: true })
const fit = new FitAddon(); term.loadAddon(fit); term.loadAddon(new WebglAddon())
term.open(ref.current!); fit.fit()
ws.onmessage = (e) => term.write(JSON.parse(e.data).chunk)  // xterm handles ANSI natively
new ResizeObserver(() => fit.fit()).observe(ref.current!)
```
Backend: stream raw bytes over the `/ws` channel from §7 and **do not strip ANSI server-side** — let xterm interpret it. If you want interactivity later you'll need a PTY (`portable-pty`) rather than plain piped stdio, since CLIs disable colour/TUI when not on a TTY; worth deciding early because retrofitting is invasive.

═══════════════════════════════════════
11. CHARTS — recommend recharts 3.9.2
═══════════════════════════════════════
Burndown, velocity, and CFD are all standard composed cartesian charts — the exact case Recharts is built for. It's React-native (declarative components, not an imperative canvas instance), peer-supports React 19, and CFD is literally a stacked area chart.
- **recharts 3.9.2** — right default. SVG, themeable via CSS, trivial to make responsive.
- **echarts 6.1.0** (+ echarts-for-react 3.0.6) — canvas, better at 10k+ points, but it's an imperative library you wrap; overkill unless boards get huge and it fights React 19/Compiler idioms.
- **@visx/visx 4.0.0** — low-level d3 primitives; maximum control, but you're building axes and tooltips yourself. Only worth it if these charts are a differentiator rather than a feature.

Recharts's SVG approach also means burndown/velocity charts stay crisp and are printable/exportable — useful for a PM tool. Revisit echarts only if you hit a CFD with years of daily datapoints.

═══════════════════════════════════════
12. TESTING — vitest 4.1.10 + @testing-library/react 16.3.2 + playwright 1.61.1
═══════════════════════════════════════
```
vitest@4.1.10 @vitest/ui@4.1.10 @vitest/coverage-v8@4.1.10
@testing-library/react@16.3.2 @testing-library/dom@10.4.1   # ← explicit peer since RTL 16
@testing-library/user-event@14.6.1 @testing-library/jest-dom@6.9.1
@playwright/test@1.61.1  msw@2.15.0  jsdom@29.1.1
```
Vitest 4 change to note: browser providers are now separate packages (`@vitest/browser-playwright`, `@vitest/browser-webdriverio`, `@vitest/browser-preview` — all 4.1.10), not inline config. RTL 16 requires `@testing-library/dom` as an explicit devDependency — it's no longer transitive.

Testing strategy for Atlas specifically: **do not try to test PDND drag in jsdom.** Native HTML5 DnD events don't exist there — this is a well-known dead end that eats days. Split it:
- Unit (vitest, jsdom): `applyMove`/`upsertCard` pure reducers — fast, exhaustive, covers the optimistic logic that actually carries risk.
- Integration (vitest + RTL + msw): the mutation lifecycle — assert optimistic apply, forced-error rollback, and onSettled invalidation.
- E2E (playwright): real drag via `page.dragAndDrop()` / manual mouse steps in a real browser, plus the action-menu keyboard path from §4.
Playwright is also where you verify "feels instant": assert the card is in the new column *before* the API response resolves (route interception with a delay) — that's a direct executable test of the CLAUDE.md non-negotiable.

═══════════════════════════════════════
13. TYPED API CLIENT — openapi-typescript 7.13.0 + openapi-fetch 0.17.0
═══════════════════════════════════════
Full chain, Rust → TS:
- **Axum 0.8.9 + utoipa 5.5.0 + utoipa-axum 0.2.0** — annotate handlers, derive `ToSchema` on DTOs, emit `openapi.json`. (aide 0.15.1 is the alternative if you prefer its style.)
- **openapi-typescript 7.13.0** — types only, zero runtime.
- **openapi-fetch 0.17.0** — ~6kB typed fetch wrapper; no generated method-per-endpoint, so the diff noise stays low.

Prefer this over **orval 8.22.0** (which generates TanStack Query hooks + MSW mocks) because orval's generated hooks won't know about your PDND-driven optimistic move logic — you'd end up hand-writing those mutations anyway and fighting regenerated code. openapi-fetch gives you types without dictating hook shape.

Wire it as an explicit, committed step — not a build-time plugin. Codegen inside `vite build` makes builds nondeterministic and requires a running backend in CI.
```jsonc
// package.json
"scripts": {
  "api:schema": "cargo run -p atlas-server --bin export-openapi > openapi.json",
  "api:types":  "openapi-typescript openapi.json -o src/api/schema.d.ts",
  "api:gen":    "npm run api:schema && npm run api:types",
  "build":      "tsc -b && vite build"
}
```
```ts
// src/api/client.ts
import createClient from 'openapi-fetch'
import type { paths } from './schema'
export const api = createClient<paths>({ baseUrl: import.meta.env.VITE_API_URL ?? '/api' })

// fully typed: path, params, body, and response all checked against the Rust types
const { data, error } = await api.GET('/api/boards/{id}', { params: { path: { id: boardId } } })
```
Commit both `openapi.json` and `schema.d.ts`. Then add a CI check that regenerates and fails on diff — that turns "Rust DTO changed, frontend silently broke" into a red build, which is the entire value of doing this. Given Atlas's recursive board model, also verify utoipa emits the self-referential Card→Board schema sanely; recursive `$ref`s are the one place these generators get weird, so check it early rather than after the model is set.

Secrets note: ensure the OpenAPI export never leaks redacted wrapper types' inner values — the schema is a public artifact and your CLAUDE.md forbids secrets in API responses.

═══════════════════════════════════════
14. REACT 19 SPECIFICS THAT CHANGE THE ABOVE
═══════════════════════════════════════
**React Compiler 1.0.0 (stable).** Turn it on from day one. It auto-memoises, including *conditionally* — which manual `useMemo` cannot do. Practical effect on this stack: stop hand-writing `useMemo`/`useCallback`/`React.memo` around board rendering; the compiler does it better and your PDND `useEffect` cleanup logic stays readable. Enforce with eslint-plugin-react-hooks 7.1.1 (ships the compiler rules) — the compiler bails out silently on rule-breaking components, so the lint rule is how you learn it bailed. Note the Vite 8 wiring change in §1 (goes through @rolldown/plugin-babel now, not a plugin option).

**Actions / useActionState / useFormStatus.** These are for uncontrolled form submissions with pending/error state. They do **not** replace TanStack Query mutations, and specifically cannot express the cancel→snapshot→rollback lifecycle in §3. Keep card-move on `useMutation`. `useActionState` is reasonable for simple card create/rename dialogs, but since you're already on react-hook-form + zod (§6), consistency argues for keeping forms there rather than splitting idioms.

**`use()` hook.** Reads a promise or context conditionally. TanStack Query already handles suspense properly via `useSuspenseQuery` — reach for that, not raw `use()`, which has no cache and will re-fire promises unless you own their identity.

**`useOptimistic`.** Tempting for the card move; resist it. It's scoped to a component's render and resets when the underlying prop settles — it has no cross-component cache, no rollback hook, and no invalidation. Your optimistic move must persist across the whole board tree and interact with WS pushes. TanStack Query's `onMutate` is the right tool; `useOptimistic` is for local single-component form feedback.

**ref as prop / cleanup functions.** `forwardRef` is no longer needed, and ref callbacks may now return a cleanup function — which pairs nicely with PDND's `combine(...)` teardown pattern shown in §4.

**Peer floors.** react-router 8 requires `react >= 19.2.7` exactly (pinned to current). @testing-library/react 16.3.2, TanStack Query 5.101.2, Router 1.170.18, recharts 3.9.2, Tiptap 3.28.0, PDND's react sub-packages, zustand 5.0.14 — all verified React 19 compatible. The only React-19-incompatible library in the entire evaluated set is react-beautiful-dnd, which is deprecated anyway.

═══════════════════════════════════════
FINAL package.json (verified 2026-07-16)
═══════════════════════════════════════
```jsonc
"dependencies": {
  "react": "19.2.7", "react-dom": "19.2.7",
  "@tanstack/react-router": "1.170.18",
  "@tanstack/react-query": "5.101.2",
  "@tanstack/react-virtual": "3.14.6",
  "@atlaskit/pragmatic-drag-and-drop": "2.0.1",
  "@atlaskit/pragmatic-drag-and-drop-hitbox": "2.0.0",
  "@atlaskit/pragmatic-drag-and-drop-auto-scroll": "3.0.0",
  "@atlaskit/pragmatic-drag-and-drop-react-drop-indicator": "4.1.0",
  "@atlaskit/pragmatic-drag-and-drop-react-accessibility": "3.1.2",
  "@atlaskit/pragmatic-drag-and-drop-live-region": "2.0.0",
  "zustand": "5.0.14", "immer": "11.1.15",
  "react-hook-form": "7.81.0", "zod": "4.4.3", "@hookform/resolvers": "5.4.0",
  "@tiptap/react": "3.28.0", "@tiptap/starter-kit": "3.28.0", "@tiptap/pm": "3.28.0",
  "@tiptap/extension-mention": "3.28.0", "@tiptap/extension-code-block-lowlight": "3.28.0",
  "lowlight": "3.3.0",
  "@xterm/xterm": "6.0.0", "@xterm/addon-fit": "0.11.0", "@xterm/addon-webgl": "0.19.0",
  "@xterm/addon-search": "0.16.0", "@xterm/addon-serialize": "0.14.0",
  "recharts": "3.9.2",
  "openapi-fetch": "0.17.0"
},
"devDependencies": {
  "vite": "8.1.5", "@vitejs/plugin-react": "6.0.3",
  "@rolldown/plugin-babel": "0.2.3", "@babel/core": "8.0.1",
  "babel-plugin-react-compiler": "1.0.0",
  "@tanstack/router-plugin": "1.168.20", "@tanstack/react-router-devtools": "1.167.0",
  "@tanstack/react-query-devtools": "5.101.2",
  "typescript": "6.0.3",              // NOT 7.0.2 — see risks
  "eslint": "10.7.0", "typescript-eslint": "8.64.0", "eslint-plugin-react-hooks": "7.1.1",
  "vitest": "4.1.10", "@vitest/ui": "4.1.10", "@vitest/coverage-v8": "4.1.10",
  "@testing-library/react": "16.3.2", "@testing-library/dom": "10.4.1",
  "@testing-library/user-event": "14.6.1", "@testing-library/jest-dom": "6.9.1",
  "@playwright/test": "1.61.1", "msw": "2.15.0", "jsdom": "29.1.1",
  "openapi-typescript": "7.13.0"
}
```

## Facts

- **[verified]** Vite is at 8.1.5. It depends on rolldown ~1.1.5 and lightningcss ^1.32.0 — Rolldown replaced BOTH esbuild (dev transforms) and Rollup (prod bundling). engines: node ^20.19.0 || >=22.12.0. ESM-only. build.rollupOptions renamed to build.rolldownOptions; Lightning CSS is now default CSS minifier (build.cssMinify:'esbuild' reverts).
  - Evidence: npm view vite version/dependencies/engines → 8.1.5, {rolldown:'~1.1.5', lightningcss:'^1.32.0'}; https://vite.dev/blog/announcing-vite8
- **[verified]** create-vite is 9.1.1; scaffold with `npm create vite@latest atlas-web -- --template react-ts` (or react-swc-ts). Note react-swc-ts is less relevant on Vite 8 since Oxc/Rolldown already handles transforms natively.
  - Evidence: npm view create-vite version → 9.1.1
- **[verified]** @vitejs/plugin-react 6.0.3 peerDependencies are {'@rolldown/plugin-babel':'^0.1.7 || ^0.2.0', 'babel-plugin-react-compiler':'^1.0.0', vite:'^8.0.0'} — v6 REQUIRES Vite 8 and no longer bundles Babel. React Compiler is wired via the exported reactCompilerPreset() helper passed to @rolldown/plugin-babel, and babel() must be listed BEFORE react().
  - Evidence: npm view @vitejs/plugin-react@6.0.3 peerDependencies; https://react.dev/learn/react-compiler/installation; dev.to/recca0120 Vite 8 + compiler writeup
- **[verified]** @rolldown/plugin-babel is 0.2.3; @babel/core is 8.0.1.
  - Evidence: npm view @rolldown/plugin-babel version → 0.2.3; npm view @babel/core version → 8.0.1
- **[verified]** React/react-dom are 19.2.7. React Compiler is 1.0.0 STABLE (babel-plugin-react-compiler@1.0.0, react-compiler-runtime@1.0.0), released 2025-10-07, battle-tested at Meta; Quest Store saw up to 12% load/nav gains and some interactions >2.5x faster.
  - Evidence: npm view react version → 19.2.7; npm view babel-plugin-react-compiler version → 1.0.0; https://react.dev/blog/2025/10/07/react-compiler-1
- **[verified]** eslint-plugin-react-hooks is 7.1.1 and ships the React Compiler lint rules; ESLint itself is 10.7.0.
  - Evidence: npm view eslint-plugin-react-hooks version → 7.1.1; npm view eslint version → 10.7.0
- **[verified]** TRAP: TypeScript `latest` on npm is 7.0.2 — the full Go native port (stable 2026-07-08, ~8-12x faster; VS Code check 77.8s→7.5s). BUT typescript-eslint 8.64.0 peer-caps at `typescript: '>=4.8.4 <6.1.0'` and has NO v9 RC published (dist-tags: rc-v8, latest 8.64.0, canary 8.64.1-alpha.3). TS 7 therefore breaks type-aware typescript-eslint today. TS 6.0.3 is the newest stable within the supported range.
  - Evidence: npm view typescript dist-tags → latest 7.0.2; npm view typescript-eslint version peerDependencies → 8.64.0, typescript '>=4.8.4 <6.1.0'; npm view typescript-eslint dist-tags (no rc-v9); npm view typescript versions → 6.x stable ['6.0.2','6.0.3']; https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/
- **[verified]** TypeScript 7's programmatic compiler API is NOT stable until ~7.1 — this is why Vue/Svelte tooling can't adopt it yet. Any tool consuming the TS compiler API (vue-tsc-likes, some Vite plugins) is at risk.
  - Evidence: https://devblogs.microsoft.com/typescript/announcing-typescript-7-0-rc/ ; theregister.com 2026-07-09 coverage
- **[verified]** react-router latest is 8.2.0 with peerDependencies {react:'>=19.2.7', react-dom:'>=19.2.7'}. The react-router-dom package was REMOVED in v8 — it is frozen at 7.18.1 and was only ever a re-export shim (its sole dependency is react-router@7.18.1). In v8 import RouterProvider/HydratedRouter from 'react-router/dom', everything else from 'react-router'.
  - Evidence: npm view react-router dist-tags → latest 8.2.0; npm view react-router@8.2.0 peerDependencies; npm view react-router-dom dist-tags → latest 7.18.1; npm view react-router-dom@7.18.1 dependencies → {react-router:'7.18.1'}; https://remix.run/blog/react-router-v8
- *[likely]* React Router v8 also requires Node 22.22.0 minimum, is ESM-only, makes middleware always-on (future.v8_middleware lifted; loaders/actions receive RouterContextProvider), and turns route module splitting on by default (splitRouteModules: true). Annual release cycle now.
  - Evidence: https://remix.run/blog/react-router-v8; https://reactrouter.com/upgrading/v7; stackmaven.io React Router 8 GA coverage
- **[verified]** @tanstack/react-router is 1.170.18; @tanstack/router-plugin 1.168.20 (peer supports vite >=8.0.0); @tanstack/react-router-devtools 1.167.0. Peer: react >=18 || >=19. It provides first-class typed search params with schema validation/defaults/structural sharing, plus a built-in SWR route loader cache.
  - Evidence: npm view @tanstack/react-router version → 1.170.18; npm view @tanstack/router-plugin peerDependencies (vite '>=5.0.0 || ... || >=8.0.0'); https://tanstack.com/router/latest/docs/overview
- **[verified]** @tanstack/react-query is 5.101.2 (query-core and devtools match). Peer: react '^18 || ^19'. Recent v5 releases pass a context object as the final arg to every mutation callback, with context.client being the QueryClient.
  - Evidence: npm view @tanstack/react-query version peerDependencies → 5.101.2, {react:'^18 || ^19'}; https://tanstack.com/query/v5/docs/framework/react/guides/optimistic-updates
- **[verified]** Canonical optimistic pattern: onMutate → cancelQueries (prevents in-flight refetch clobbering the optimistic write) → getQueryData snapshot → setQueryData → return {previous} as context; onError → setQueryData(previous) rollback; onSettled → invalidateQueries (must be onSettled, not onSuccess, so it also re-syncs after a rollback).
  - Evidence: https://tanstack.com/query/v5/docs/framework/react/guides/optimistic-updates
- **[verified]** DECISIVE: react-beautiful-dnd@13.1.1 carries an npm deprecation notice ('react-beautiful-dnd is now deprecated', issue #2672) and peer-caps at react '^16.8.5 || ^17.0.0 || ^18.0.0' — it cannot install on React 19. Disqualified.
  - Evidence: npm view react-beautiful-dnd@13.1.1 deprecated + peerDependencies
- **[verified]** DECISIVE: @dnd-kit/core@6.3.1 last published 2024-12-05 (19 months stale as of 2026-07-16) and still declares only react '>=16.8.0'. Its rewrite @dnd-kit/react is actively developed (published 2026-07-13) but is still 0.5.0 — pre-1.0 with an unstable API surface.
  - Evidence: npm view @dnd-kit/core time.modified → 2024-12-05T17:10:20Z; npm view @dnd-kit/react version/time.modified → 0.5.0 / 2026-07-13; npm view @dnd-kit/react dist-tags → beta 0.5.1-beta-20260713030121
- **[verified]** @atlaskit/pragmatic-drag-and-drop is 2.0.1, published 2026-06-17. Its core has NO react peer dependency at all — deps are only {@babel/runtime, bind-event-listener ^3, raf-schd ^4}. 4.7kB core. Explicitly powers Trello, Jira and Confluence in production.
  - Evidence: npm view @atlaskit/pragmatic-drag-and-drop@2.0.1 dependencies/peerDependencies + time.modified; https://github.com/atlassian/pragmatic-drag-and-drop
- **[verified]** PDND optional packages (all current): -auto-scroll@3.0.0 (peers core ^2.0.0), -hitbox@2.0.0, -react-drop-indicator@4.1.0 (peer react ^18.2.0 || ^19.0.0), -live-region@2.0.0, -flourish@3.0.1, -react-accessibility@3.1.2 (peer react ^18.2.0 || ^19.0.0, published 2026-07-16 — i.e. today). Active maintenance is unambiguous.
  - Evidence: npm view for each @atlaskit/pragmatic-drag-and-drop-* package (version, peerDependencies, time.modified)
- **[verified]** CORRECTION to a widely-repeated blog claim: PDND does NOT provide keyboard dragging. Blogs assert '@atlaskit/pragmatic-drag-and-drop-react-accessibility gives Space to pick up, arrows to move, Enter to drop' — this is false. PDND builds on native HTML5 DnD, and no browser implements the HTML5 spec's keyboard event model. Atlassian's documented guidance: 'Every draggable item should have an accessible way to achieve the same outcome without using drag and drop', and their user testing found action menus ('Move Up'/'Move Down'/'Move to column') superior to directional keyboard nav — cheaper, more discoverable, more reliable with screen readers. DragHandleButton is a dual-purpose control: drag handle for pointer, menu trigger for keyboard.
  - Evidence: https://deepwiki.com/atlassian/pragmatic-drag-and-drop/6.4-accessibility-guidelines (fetched directly); contradicts pkgpulse/hookedonui secondary claims; npm description of -react-accessibility ('react components to assist with setting up accessible experiences' — not a keyboard drag engine)
- **[verified]** @hello-pangea/dnd@18.0.1 is the maintained rbd fork and DOES support React 19 (peer '^18.0.0 || ^19.0.0') with rbd's built-in keyboard dragging — but it last published 2025-02-09 (17 months stale) and inherits rbd's known perf ceiling on large boards.
  - Evidence: npm view @hello-pangea/dnd@18.0.1 peerDependencies + time.modified
- **[verified]** zustand is 5.0.14 (optional peers: react >=18, immer >=9.0.6, use-sync-external-store >=1.2.0 — all optional). jotai is 2.20.2. immer is 11.1.15.
  - Evidence: npm view zustand version peerDependencies → 5.0.14; npm view jotai version → 2.20.2; npm view immer version → 11.1.15
- **[verified]** react-hook-form is 7.81.0, zod is 4.4.3, @hookform/resolvers is 5.4.0. Zod 4 is a major rewrite (faster, smaller); resolvers v5 is the version that carries Zod 4 support — do not mix with resolvers v3.
  - Evidence: npm view react-hook-form/zod/@hookform/resolvers version → 7.81.0 / 4.4.3 / 5.4.0
- **[verified]** @tanstack/react-virtual is 3.14.6, peer react ^16.8 || ^17 || ^18 || ^19. Headless, works with dynamic row heights via measureElement.
  - Evidence: npm view @tanstack/react-virtual version peerDependencies → 3.14.6
- **[verified]** Tiptap is 3.28.0 across @tiptap/react, @tiptap/starter-kit, @tiptap/pm, @tiptap/extension-mention, @tiptap/extension-code-block-lowlight. Peer react includes ^19.0.0. It is ProseMirror-based — the same engine underneath Jira's ADF.
  - Evidence: npm view @tiptap/react@3.28.0 peerDependencies + version checks on each package
- *[likely]* Tiptap licensing: core editor + extensions are MIT and free forever; Atlassian-style Mention/CodeBlock/etc. are MIT. In June 2025 Tiptap open-sourced 10 formerly-Pro extensions under MIT. Only Comments/Snapshots/AI remain Pro, and those require the paid Cloud Platform ($49/mo Start, $149 Team, $999 Business; free plan removed June 2025). For Atlas (self-hosted), the MIT surface covers mentions, code blocks, checklists (TaskList), images, and markdown.
  - Evidence: https://news.ycombinator.com/item?id=44202103; https://tiptap.dev/pricing; https://tiptap.dev/blog/release-notes/tiptaps-new-pricing-model-is-live
- **[verified]** Lexical is 0.48.0 (still 0.x after years — Meta's editor, no 1.0). Plate is now published as `platejs`/@platejs/core at 53.2.4 (the old @udecode/plate is frozen at 49.0.0) — note Plate is Slate-based, not ProseMirror-based.
  - Evidence: npm view lexical version → 0.48.0; npm view platejs version → 53.2.4; npm view @udecode/plate version → 49.0.0
- **[verified]** xterm.js moved to the @xterm scope: @xterm/xterm is 6.0.0 (published 2026-07-15 — actively maintained). The unscoped `xterm` package is frozen at 5.3.0 and must not be used. Addons: @xterm/addon-fit 0.11.0, @xterm/addon-webgl 0.19.0, @xterm/addon-search 0.16.0, @xterm/addon-serialize 0.14.0.
  - Evidence: npm view @xterm/xterm version time.modified → 6.0.0 / 2026-07-15; npm view xterm version → 5.3.0; addon version checks
- **[verified]** recharts is 3.9.2 (peer react includes ^19.0.0). echarts is 6.1.0 with echarts-for-react 3.0.6. @visx/visx is 4.0.0. lowlight (for Tiptap code highlighting) is 3.3.0.
  - Evidence: npm view recharts@3.9.2 peerDependencies; npm view echarts/echarts-for-react/@visx/visx/lowlight version
- **[verified]** Testing: vitest 4.1.10 (peer vite '^6 || ^7 || ^8'), @vitest/ui + @vitest/coverage-v8 + @vitest/browser all 4.1.10. Vitest 4 splits browser providers into separate packages: @vitest/browser-playwright, @vitest/browser-webdriverio, @vitest/browser-preview (all 4.1.10). @testing-library/react 16.3.2 (peer react ^18 || ^19, requires @testing-library/dom ^10 as an explicit peer — 10.4.1). @testing-library/user-event 14.6.1, @testing-library/jest-dom 6.9.1. playwright/@playwright/test 1.61.1. jsdom 29.1.1, happy-dom 20.10.6, msw 2.15.0.
  - Evidence: npm view vitest@4.1.10 peerDependencies; version checks on each package
- **[verified]** openapi-typescript is 7.13.0 (bin: openapi-typescript, uses @redocly/openapi-core ^1.34.6) and pairs with openapi-fetch 0.17.0 (~6kB runtime, no codegen of client methods — types only). Alternatives: orval 8.22.0 (generates TanStack Query hooks + MSW mocks), @hey-api/openapi-ts 0.99.0.
  - Evidence: npm view openapi-typescript@7.13.0 bin dependencies; npm view openapi-fetch/orval/@hey-api/openapi-ts version
- **[verified]** Rust side for OpenAPI emission: axum 0.8.9, utoipa 5.5.0, utoipa-axum 0.2.0, utoipa-swagger-ui 9.0.2; aide 0.15.1 is the alternative. tokio-tungstenite 0.30.0 for WS (though axum has native WebSocketUpgrade).
  - Evidence: crates.io API max_stable_version for axum/utoipa/utoipa-axum/utoipa-swagger-ui/aide/tokio-tungstenite
- **[verified]** Vite dev proxy supports WebSockets via `ws: true` and `rewriteWsOrigin`; keys beginning with ^ are treated as RegExp. Env: only VITE_-prefixed vars are exposed on import.meta.env (configurable via envPrefix, default 'VITE_'); load order is .env → .env.local → .env.[mode] → .env.[mode].local; type via /// <reference types="vite/client" /> plus an ImportMetaEnv interface.
  - Evidence: https://vite.dev/config/server-options and https://vite.dev/guide/env-and-mode (both fetched)
- **[verified]** oxlint is 1.74.0 — a viable Rust-based lint path if Atlas chooses TypeScript 7 and has to abandon type-aware typescript-eslint. vite-plugin-checker is 0.14.4 (peer typescript '*').
  - Evidence: npm view oxlint version → 1.74.0; npm view vite-plugin-checker version peerDependencies → 0.14.4

## Risks

- TYPESCRIPT 7 IS A TRAP RIGHT NOW. `npm i -D typescript` installs 7.0.2 (the Go port) because it is tagged `latest`, but typescript-eslint 8.64.0 peer-caps at `typescript >=4.8.4 <6.1.0` and has NO v9 RC published (dist-tags show only rc-v8/latest/canary). Installing TS 7 silently breaks type-aware linting. Pin `"typescript": "6.0.3"` explicitly in package.json — do not use a caret, or a future `npm update` drags you to 7. Revisit when typescript-eslint ships TS7 support; oxlint 1.74.0 is the escape hatch if you want TS 7's ~10x checking now. Related: TS 7's programmatic compiler API isn't stable until ~7.1, which is why Vue/Svelte tooling can't adopt it.
- PDND HAS NO KEYBOARD DRAGGING — and the internet says otherwise. Multiple blogs (pkgpulse, hookedonui) claim @atlaskit/pragmatic-drag-and-drop-react-accessibility provides Space-to-lift/arrow-to-move/Enter-to-drop and 'a perfect Accessibility score'. Atlassian's own guidelines contradict this: PDND rides native HTML5 DnD, and no browser implements the HTML5 keyboard model. You must build 'Move to column…' action menus yourself. This is the Jira-authentic pattern (Jira does exactly this) and Atlassian's user testing says menus beat arrow-key DnD — but it is unbudgeted work if you assumed a keyboard sensor. If arrow-key dragging is a hard requirement, @hello-pangea/dnd 18.0.1 is the only live library that has it, at the cost of a 17-month-stale dependency and rbd's perf ceiling.
- VITE 8 + REACT COMPILER WIRING IS NEW AND MOST TUTORIALS ARE WRONG. @vitejs/plugin-react v6 dropped built-in Babel, so the old `react({ babel: { plugins: [['babel-plugin-react-compiler']] } })` snippet — which is what almost every blog and most model-generated configs still show — silently does nothing on Vite 8. You must use `babel({ presets: [reactCompilerPreset()] })` from @rolldown/plugin-babel, listed BEFORE `react()`. Verify the compiler is actually running (check for `react-compiler-runtime` imports / _c cache slots in output) rather than assuming.
- VITE 8 MIGRATION EDGES: build.rollupOptions → build.rolldownOptions; CJS interop changed (legacy.inconsistentCjsInterop:true is the temporary escape); Yarn PNP is incompatible; Lightning CSS is now the default minifier (build.cssMinify:'esbuild' reverts). Node floor is ^20.19 || >=22.12. Rolldown is young — if a niche Rollup plugin misbehaves, the documented fallback is rolldown-vite on the Vite 7 API surface.
- DND-KIT IS QUIETLY BIFURCATED. @dnd-kit/core 6.3.1 (the version every tutorial and LLM suggests) last published 2024-12-05 — 19 months stale — and @dnd-kit/sortable is at a mismatched 10.0.0. The active successor @dnd-kit/react is 0.5.0, pre-1.0, with a different API. Choosing dnd-kit today means picking between an abandoned stable and an unstable rewrite. This asymmetry vs PDND (sub-package published today, and Atlassian cannot abandon it without breaking Jira) is the core of the recommendation.
- REACT-ROUTER-DOM IS GONE IN V8 AND THIS WILL CONFUSE TOOLING. It's frozen at 7.18.1 as a re-export shim. Every existing snippet importing from 'react-router-dom' is now wrong for v8 (RouterProvider moved to 'react-router/dom'). If you go TanStack Router as recommended this is moot — but it's a live hazard for any copied example code, and LLM-suggested code will get this wrong for a long while.
- OPTIMISTIC MOVE CORRECTNESS IS SUBTLE — three specific failure modes: (1) invalidating in onSuccess instead of onSettled leaves the client authoritative after a rollback (silent desync); (2) skipping cancelQueries lets an in-flight GET resolve after the optimistic write and snap the card back visibly; (3) a WS push landing mid-mutation overwrites the optimistic state. Guard with qc.isMutating() and echo-suppress via actorId. Also add reconnect resync (invalidate on WS reopen) or clients silently drift after a dropped connection.
- USE INTEGER INDICES FOR CARD ORDER AND YOU WILL REGRET IT. Integer positions force a full-column renumber per move (SQLite write amplification) and make concurrent moves conflict. Use a fractional/lexicographic rank (LexoRank-style), as Jira does — one-row UPDATE per move, conflict-free. This is a schema decision that is painful to retrofit, so settle it before the board model is written.
- TIPTAP COMMENTS ARE A CLOUD DEPENDENCY. Core + mentions + code blocks + checklists + images are all MIT and fine for self-hosted Atlas. But Comments/Snapshots/AI are Pro and require the paid Cloud Platform ($49/mo+; free tier removed June 2025). Card comments must be your own Axum entities with plain Tiptap editors — reaching for Tiptap's Comments extension would put a SaaS dependency inside a self-hosted app. Separately, markdown *paste* relies on the third-party tiptap-markdown 0.9.0, whose maintenance I'd treat as uncertain; a custom paste handler is more predictable.
- XTERM CHOICE HINGES ON PTY, NOT ON THE FRONTEND. If the `claude` subprocess is spawned with plain piped stdio, it will detect it isn't a TTY and disable colour/spinners — so xterm.js renders a plain dull log and you've paid for a terminal emulator you aren't using. Getting authentic ANSI streaming requires a PTY (portable-pty) on the Rust side. Decide this early; retrofitting stdio→PTY is invasive. Also: use @xterm/* scoped packages — unscoped `xterm` is frozen at 5.3.0 and will look current to a careless install.
- VIRTUALISATION AND DRAG-AND-DROP ARE ANTAGONISTIC. Offscreen rows aren't in the DOM, so they can't register as PDND drop targets, and auto-scroll reveals blanks. Do not virtualise board columns (rarely >50 cards). Virtualise only the backlog and log views. Where both are unavoidable, generous overscan plus the action-menu fallback is the mitigation — another reason that menu is not optional.
- DON'T TEST PDND DRAG IN JSDOM. Native HTML5 drag events don't exist there; this is a well-known time sink. Test pure reducers in vitest, the mutation lifecycle with RTL+msw, and real drags only in Playwright. Also note Vitest 4 moved browser providers to separate packages (@vitest/browser-playwright etc.) and RTL 16 needs @testing-library/dom as an explicit devDependency.
- SECRETS AND VITE_ ARE FUNDAMENTALLY INCOMPATIBLE. Any VITE_-prefixed var is inlined into the client bundle in plaintext. Given CLAUDE.md's hard non-negotiable that PATs/API keys are encrypted at rest and never appear in logs/Debug/API responses, no secret may ever carry the VITE_ prefix. Recommend a CI grep for VITE_.*(TOKEN|KEY|SECRET|PAT). Likewise audit that the exported openapi.json doesn't leak redacted wrapper types' inner values — it's a public build artifact.
- RECURSIVE BOARDS WILL STRESS OPENAPI CODEGEN. Atlas's Card→Board→Card self-reference is exactly where utoipa's schema emission and openapi-typescript's $ref resolution get weird (infinite expansion or `unknown`). Validate the generated schema.d.ts against the recursive model early — before the domain model hardens — rather than discovering it once boards-in-cards is half-built.
- UNVERIFIED CLAIMS I'D FLAG: react-router v8's Node 22.22 floor and always-on middleware come from release-blog/secondary coverage rather than a direct package-metadata check (its peerDeps only pin react/react-dom >=19.2.7) — confirm against reactrouter.com before relying on it. Tiptap's exact free/Pro extension boundary is from HN/pricing-page coverage; re-verify the specific extensions you depend on against tiptap.dev before committing, since that boundary has moved twice (Jun 2025 open-sourcing, new pricing model).
