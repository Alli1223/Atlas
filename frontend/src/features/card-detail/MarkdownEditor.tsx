import { CodeBlockLowlight } from '@tiptap/extension-code-block-lowlight'
import { TaskItem, TaskList } from '@tiptap/extension-list'
import { Mention } from '@tiptap/extension-mention'
import { Placeholder } from '@tiptap/extensions'
import { EditorContent, useEditor } from '@tiptap/react'
import { StarterKit } from '@tiptap/starter-kit'
import { common, createLowlight } from 'lowlight'
import {
  Bold,
  Code,
  CodeSquare,
  Heading1,
  Heading2,
  Heading3,
  Italic,
  Link2,
  List,
  ListChecks,
  ListOrdered,
  Quote,
  Strikethrough,
} from 'lucide-react'
import { type ReactNode, useCallback } from 'react'

import { Button } from '@/components/ui'
import { cx } from '@/lib/cx'
import { ICON_SMALL } from '@/lib/icon'

import styles from './MarkdownEditor.module.css'
import { serializeMarkdown } from './markdown'
import { parseMarkdown } from './markdown'
import { type MentionCandidate, mentionSuggestion } from './mention-suggestion'

const lowlight = createLowlight(common)

export interface MarkdownEditorProps {
  /** Initial markdown source. Parsed to a document for editing. */
  value: string
  /** Called with markdown source when the user saves. */
  onSave: (markdown: string) => void
  /** Called when the user abandons the edit. */
  onCancel?: () => void
  /** People the @-menu can offer. */
  mentionCandidates?: MentionCandidate[]
  /** Shown when the document is empty. */
  placeholder?: string
  /** True while a save is in flight — disables the buttons. */
  isSaving?: boolean
  /** Hides the Save/Cancel row (for a always-editing surface like a new comment). */
  hideActions?: boolean
  /** Label on the primary button. @default 'Save' */
  saveLabel?: string
  /** A stable id so the editor can be found and focused. */
  autoFocus?: boolean
}

/**
 * The rich-text surface — TipTap over the markdown converters.
 *
 * TipTap is a ProseMirror editor, so it edits a document, not text. `parseMarkdown` turns
 * the stored source into that document on load and `serializeMarkdown` turns it back on save
 * — the app never holds rendered HTML, per the storage rule. `codeBlock` is disabled in
 * StarterKit so `CodeBlockLowlight` can own fenced blocks with syntax highlighting; the
 * markdown serialiser flattens either back to a plain ``` fence.
 */
export function MarkdownEditor({
  value,
  onSave,
  onCancel,
  mentionCandidates = [],
  placeholder = 'Write something…',
  isSaving = false,
  hideActions = false,
  saveLabel = 'Save',
  autoFocus = false,
}: MarkdownEditorProps) {
  const editor = useEditor({
    extensions: [
      StarterKit.configure({
        codeBlock: false,
        link: { openOnClick: false, autolink: true },
      }),
      CodeBlockLowlight.configure({ lowlight }),
      TaskList,
      TaskItem.configure({ nested: true }),
      Mention.configure({
        HTMLAttributes: { class: styles.mention },
        suggestion: mentionSuggestion(() => mentionCandidates),
      }),
      Placeholder.configure({ placeholder }),
    ],
    content: parseMarkdown(value),
    autofocus: autoFocus ? 'end' : false,
    editorProps: {
      attributes: { class: styles.content ?? '', 'aria-label': placeholder },
    },
  })

  const save = useCallback(() => {
    if (!editor) return
    onSave(serializeMarkdown(editor.getJSON()))
  }, [editor, onSave])

  if (!editor) return null

  return (
    <div className={styles.editor}>
      <div className={styles.toolbar} role="toolbar" aria-label="Formatting">
        <ToolButton
          label="Bold"
          isActive={editor.isActive('bold')}
          onClick={() => editor.chain().focus().toggleBold().run()}
          icon={<Bold {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Italic"
          isActive={editor.isActive('italic')}
          onClick={() => editor.chain().focus().toggleItalic().run()}
          icon={<Italic {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Strikethrough"
          isActive={editor.isActive('strike')}
          onClick={() => editor.chain().focus().toggleStrike().run()}
          icon={<Strikethrough {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Inline code"
          isActive={editor.isActive('code')}
          onClick={() => editor.chain().focus().toggleCode().run()}
          icon={<Code {...ICON_SMALL} aria-hidden="true" />}
        />
        <span className={styles.divider} aria-hidden="true" />
        <ToolButton
          label="Heading 1"
          isActive={editor.isActive('heading', { level: 1 })}
          onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}
          icon={<Heading1 {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Heading 2"
          isActive={editor.isActive('heading', { level: 2 })}
          onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
          icon={<Heading2 {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Heading 3"
          isActive={editor.isActive('heading', { level: 3 })}
          onClick={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}
          icon={<Heading3 {...ICON_SMALL} aria-hidden="true" />}
        />
        <span className={styles.divider} aria-hidden="true" />
        <ToolButton
          label="Bullet list"
          isActive={editor.isActive('bulletList')}
          onClick={() => editor.chain().focus().toggleBulletList().run()}
          icon={<List {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Numbered list"
          isActive={editor.isActive('orderedList')}
          onClick={() => editor.chain().focus().toggleOrderedList().run()}
          icon={<ListOrdered {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Checklist"
          isActive={editor.isActive('taskList')}
          onClick={() => editor.chain().focus().toggleTaskList().run()}
          icon={<ListChecks {...ICON_SMALL} aria-hidden="true" />}
        />
        <span className={styles.divider} aria-hidden="true" />
        <ToolButton
          label="Quote"
          isActive={editor.isActive('blockquote')}
          onClick={() => editor.chain().focus().toggleBlockquote().run()}
          icon={<Quote {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Code block"
          isActive={editor.isActive('codeBlock')}
          onClick={() => editor.chain().focus().toggleCodeBlock().run()}
          icon={<CodeSquare {...ICON_SMALL} aria-hidden="true" />}
        />
        <ToolButton
          label="Link"
          isActive={editor.isActive('link')}
          onClick={() => toggleLink(editor)}
          icon={<Link2 {...ICON_SMALL} aria-hidden="true" />}
        />
      </div>

      <EditorContent editor={editor} className={styles.editorSurface} />

      {!hideActions && (
        <div className={styles.actions}>
          <Button appearance="primary" size="compact" onClick={save} isLoading={isSaving}>
            {saveLabel}
          </Button>
          {onCancel && (
            <Button appearance="subtle" size="compact" onClick={onCancel} disabled={isSaving}>
              Cancel
            </Button>
          )}
        </div>
      )}
    </div>
  )
}

/** Reads a serialiser off a live editor — used by callers that own the Save button. */
export function editorMarkdown(editorJson: unknown): string {
  return serializeMarkdown(editorJson as Parameters<typeof serializeMarkdown>[0])
}

interface ToolButtonProps {
  label: string
  isActive: boolean
  onClick: () => void
  icon: ReactNode
}

function ToolButton({ label, isActive, onClick, icon }: ToolButtonProps) {
  return (
    <button
      type="button"
      className={cx(styles.tool, isActive && styles.toolActive)}
      aria-label={label}
      aria-pressed={isActive}
      title={label}
      // mousedown-preventDefault keeps the editor selection while the button is pressed.
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
    >
      {icon}
    </button>
  )
}

/** Toggles a link, prompting for a URL when adding one. */
function toggleLink(editor: ReturnType<typeof useEditor>): void {
  if (!editor) return
  if (editor.isActive('link')) {
    editor.chain().focus().unsetLink().run()
    return
  }
  const url = window.prompt('Link URL')?.trim()
  if (!url) return
  // Only linkify a real selection — a link mark with no text is invisible and unremovable.
  if (editor.state.selection.empty) {
    editor.chain().focus().insertContent(`[${url}](${url})`).run()
    return
  }
  editor.chain().focus().setLink({ href: url }).run()
}
