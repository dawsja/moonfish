// Notes on lines of a diff or a file, written in the viewer and handed to the chat as one
// message. Drafts only: kept in memory per workspace, never saved, gone once they're in the
// composer. Each quotes its code, so the message still makes sense after the file changes.

/** A line as the viewer shows it: added, deleted or unchanged (all of a file's lines are), or a
 *  diff's hunk header. */
export type ViewLine = { cls: "a" | "d" | "h" | "c"; text: string; old?: number; new?: number }

/** Where a note was made: a diff of the working tree or the index, a commit's, or the file. */
export type NoteFrom = { kind: "diff"; staged: boolean } | { kind: "commit"; hash: string } | { kind: "file" }

export type Note = {
  id: string
  path: string
  from: NoteFrom
  /** The lines it's about as they were when it was made (never a hunk header). */
  lines: ViewLine[]
  comment: string
}

/** Notes with something written in them: what's counted and sent. */
export const written = (notes: readonly Note[] | undefined): Note[] => (notes ?? []).filter((n) => n.comment.trim())

/** The rows from `anchor` to `focus` (indices, either way round), kept inside `anchor`'s hunk so
 *  the lines are contiguous. */
export function span(rows: readonly ViewLine[], anchor: number, focus: number): [number, number] {
  const step = focus >= anchor ? 1 : -1
  let end = anchor
  while (end !== focus && rows[end + step] && rows[end + step].cls !== "h") end += step
  return step > 0 ? [anchor, end] : [end, anchor]
}

const num = (l: ViewLine) => (l.cls === "d" ? l.old : (l.new ?? l.old))

/** Whether a line can take a note: a real line, not a hunk header or the "new file mode" and such
 *  that a diff shows before its first hunk (numbered 0). */
export const takesNote = (l: ViewLine | undefined): boolean => !!l && l.cls !== "h" && (num(l) ?? 0) > 0
// An added line is the same line as it in the file now: a note follows the code between views.
const same = (a: ViewLine, b: ViewLine) => (a.cls === "d") === (b.cls === "d") && a.text === b.text && num(a) === num(b)

/** Where `note`'s lines are in `rows` now (first and last index), or null when they aren't
 *  there any more: changed, moved, or not part of this view. */
export function locate(rows: readonly ViewLine[], note: Note): [number, number] | null {
  const n = note.lines.length
  if (!n) return null
  for (let i = 0; i + n <= rows.length; i++) {
    if (!same(rows[i], note.lines[0])) continue
    let j = 1
    while (j < n && same(rows[i + j], note.lines[j])) j++
    if (j === n) return [i, i + n - 1]
  }
  return null
}

/** The line numbers a note is about: the file's as it is now, or for removed lines only, as
 *  they were. */
function numbers(note: Note): { lo: number; hi: number; removed: boolean } {
  const now = note.lines.flatMap((l) => (l.cls !== "d" && l.new !== undefined ? [l.new] : []))
  const nums = now.length ? now : note.lines.flatMap((l) => (l.old !== undefined ? [l.old] : []))
  return { lo: Math.min(...nums), hi: Math.max(...nums), removed: !now.length }
}

/** For the note's card: "Line 12", "Lines 10–14", "Removed line 7". */
export function lineLabel(note: Note): string {
  const { lo, hi, removed } = numbers(note)
  const which = lo === hi ? `line ${lo}` : `lines ${lo}-${hi}`
  return removed ? `Removed ${which}` : which[0].toUpperCase() + which.slice(1)
}

/** `` `path:12` ``, `` `path:10-14` ``, plus whether the lines are removed ones or a commit's. */
export function noteRef(note: Note): string {
  const { lo, hi, removed } = numbers(note)
  let ref = `\`${note.path}:${lo === hi ? lo : `${lo}-${hi}`}\``
  if (removed) ref += " (removed)"
  if (note.from.kind === "commit") ref += ` in commit ${note.from.hash.slice(0, 8)}`
  return ref
}

/** A code block's language from the file name (the chat highlights it; unknown ones are left). */
function language(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1)
  const dot = name.lastIndexOf(".")
  const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : ""
  return /^[a-z0-9+#]{1,8}$/.test(ext) ? ext : ""
}

/** The note's lines in a code block: as code when they're all old or all current, as a diff
 *  (with + and −) when they remove some lines and keep or add others. */
export function quote(note: Note): string {
  const removes = note.lines.some((l) => l.cls === "d")
  const mixed = removes && note.lines.some((l) => l.cls !== "d")
  const mark = (l: ViewLine) => (!mixed ? "" : l.cls === "a" ? "+" : l.cls === "d" ? "-" : " ")
  const body = note.lines.map((l) => mark(l) + l.text.replace(/\r$/, "")).join("\n")
  const longest = Math.max(0, ...(body.match(/`+/g) ?? []).map((r) => r.length))
  const fence = "`".repeat(Math.max(3, longest + 1))
  return `${fence}${mixed ? "diff" : language(note.path)}\n${body}\n${fence}`
}

/** One message for the chat: each written note as its place, its code, and what was said
 *  about it. A file's notes go together, top to bottom; files in the order they were noted. */
export function formatNotes(notes: readonly Note[]): string {
  const ready = written(notes)
  if (!ready.length) return ""
  const paths = [...new Set(ready.map((n) => n.path))]
  const first = (n: Note) => numbers(n).lo
  const ordered = paths.flatMap((p) => ready.filter((n) => n.path === p).sort((a, b) => first(a) - first(b)))
  return ordered.map((n) => `${noteRef(n)}\n${quote(n)}\n${n.comment.trim()}`).join("\n\n") + "\n"
}
