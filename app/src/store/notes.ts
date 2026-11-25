// Line notes being written, per workspace (`key(scope)` in ./git): in memory only, dropped
// once they're handed to the chat or the branch changes. See lib/notes.

import { create } from "zustand"
import { written, type Note } from "@/lib/notes"

type NotesStore = {
  /** Each workspace's notes, in the order they were started. */
  byScope: Record<string, Note[]>
  /** Starts a note (nothing written yet); its id. */
  add(k: string, note: Omit<Note, "id">): string
  write(k: string, id: string, comment: string): void
  remove(k: string, id: string): void
  /** Drops all of a workspace's notes; how many had something written. */
  clear(k: string): number
  /** The workspace's written notes, for the chat: none are left here afterwards. */
  take(k: string): Note[]
}

let next = 0

export const useNotes = create<NotesStore>((set, get) => {
  const put = (k: string, notes: Note[]) =>
    set((s) => {
      const byScope = { ...s.byScope }
      if (notes.length) byScope[k] = notes
      else delete byScope[k]
      return { byScope }
    })
  const of = (k: string) => get().byScope[k] ?? []
  return {
    byScope: {},
    add(k, note) {
      const id = `note-${++next}`
      put(k, [...of(k), { ...note, id }])
      return id
    },
    write(k, id, comment) {
      if (of(k).some((n) => n.id === id)) put(k, of(k).map((n) => (n.id === id ? { ...n, comment } : n)))
    },
    remove(k, id) {
      if (of(k).some((n) => n.id === id)) put(k, of(k).filter((n) => n.id !== id))
    },
    clear(k) {
      const n = written(of(k)).length
      if (get().byScope[k]) put(k, [])
      return n
    },
    take(k) {
      const notes = written(of(k))
      if (get().byScope[k]) put(k, [])
      return notes
    },
  }
})

/** How many written notes the workspace has (on `path`, if given). */
export function useNoteCount(k: string | null, path?: string): number {
  return useNotes((s) => (k ? written(s.byScope[k]).filter((n) => path === undefined || n.path === path).length : 0))
}
