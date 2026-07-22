import { create } from 'zustand'

export type ToastAppearance = 'error' | 'success' | 'info'

export interface Toast {
  id: number
  appearance: ToastAppearance
  message: string
}

interface ToastState {
  toasts: Toast[]
  push: (appearance: ToastAppearance, message: string) => void
  dismiss: (id: number) => void
}

let nextId = 1

/**
 * A tiny toast queue. Lives in the board feature because that is the only thing that needs
 * it in this phase — a snap-back on an illegal drop must *say why*, never silently eat the
 * move. A shared app-wide toaster can lift this later; the shape is deliberately generic.
 *
 * A store rather than context so a PDND monitor callback or a mutation's `onError` — neither
 * of which is a React component — can raise a toast without a hook.
 */
export const useToasts = create<ToastState>((set) => ({
  toasts: [],
  push: (appearance, message) => {
    const id = nextId++
    set((state) => ({ toasts: [...state.toasts, { id, appearance, message }] }))
  },
  dismiss: (id) => set((state) => ({ toasts: state.toasts.filter((t) => t.id !== id) })),
}))

/** Raises a toast from anywhere, including outside React. */
export function toast(appearance: ToastAppearance, message: string): void {
  useToasts.getState().push(appearance, message)
}
