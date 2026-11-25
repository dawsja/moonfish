import { Fragment, memo, useEffect, useMemo, useRef, useState } from "react"
import { MessageSquare, Pencil, X } from "lucide-react"
import { Tip } from "@/components/Tip"
import { lineLabel, locate, span, takesNote, type Note, type NoteFrom, type ViewLine } from "@/lib/notes"
import { cn } from "@/lib/utils"
import { useNotes } from "@/store/notes"

const EMPTY: Note[] = []

/** Lines being chosen: where the press (or Shift+arrows) started, and where it is now. */
type Sel = { anchor: number; focus: number }

/** A diff's or a file's lines in the viewer, as `DiffLines` and the plain file view show them,
 *  where a line number takes a note: click one, drag or Shift+click for several (or arrows,
 *  Shift+arrows and Enter on a number). Nothing changes on screen until a number is hovered. */
export function NoteLines({ rows, layout, scopeKey, path, from, target, className }: {
  rows: ViewLine[]
  layout: "diff" | "file"
  /** The workspace the notes are kept under (`key(scope)`). */
  scopeKey: string
  path: string
  from: NoteFrom
  /** The file view's line to show (a search hit): scrolled to and marked. */
  target?: number
  className?: string
}) {
  // Which of this file's notes there are, not what they say: a note's lines never change once
  // it's made, so typing in one redraws its card, not every line of a long file.
  const ids = useNotes((s) => (s.byScope[scopeKey] ?? EMPTY).flatMap((n) => (n.path === path ? [n.id] : [])).join(" "))
  const mine = useMemo(() => {
    const want = new Set(ids.split(" "))
    return (useNotes.getState().byScope[scopeKey] ?? EMPTY).filter((n) => want.has(n.id))
  }, [ids, scopeKey])
  const [editing, setEditing] = useState<string | null>(null)
  const [sel, setSel] = useState<Sel | null>(null)
  const [cursor, setCursor] = useState<number | null>(null)
  // The lines being chosen, also as of now: a release can come before the press has rendered.
  const selRef = useRef<Sel | null>(null)
  const choose = (next: Sel | null) => {
    selRef.current = next
    setSel(next)
  }
  const dragging = useRef(false)
  const lastAnchor = useRef<number | null>(null)
  const table = useRef<HTMLTableElement>(null)
  const targetRow = useRef<HTMLTableRowElement>(null)
  const fromKey = JSON.stringify(from)

  // Where each of this file's notes is now. One whose lines changed under it (the agent is still
  // at work) shows above them, if it was made in this view; it's still sent.
  const { after, marks, lost } = useMemo(() => {
    const after = new Map<number, Note[]>()
    const marks: ("noted" | "first")[] = []
    const lost: Note[] = []
    for (const n of mine) {
      const at = locate(rows, n)
      if (!at) {
        if (JSON.stringify(n.from) === fromKey || n.id === editing) lost.push(n)
        continue
      }
      after.set(at[1], [...(after.get(at[1]) ?? []), n])
      marks[at[0]] = "first"
      for (let i = at[0] + 1; i <= at[1]; i++) marks[i] ??= "noted"
    }
    return { after, marks, lost }
  }, [mine, rows, fromKey, editing])

  const range = sel ? span(rows, sel.anchor, sel.focus) : null
  const commentable = (i: number | null): i is number => i !== null && takesNote(rows[i])
  const first = rows.findIndex((l) => takesNote(l))
  const tabRow = commentable(cursor) ? cursor : target && commentable(target - 1) ? target - 1 : first

  const focusRow = (i: number) => {
    setCursor(i)
    table.current?.querySelector<HTMLElement>(`tr[data-row="${i}"] [data-line]`)?.focus()
  }

  /** A new note on rows `lo`..`hi`, open to write in. */
  const start = (lo: number, hi: number) => {
    const lines = rows.slice(lo, hi + 1).filter((l) => takesNote(l)).map((l) => ({ ...l }))
    if (!lines.length) return
    const store = useNotes.getState()
    // One left open with nothing in it gives way to this one.
    const open = store.byScope[scopeKey]?.find((n) => n.id === editing)
    if (open && !open.comment.trim()) store.remove(scopeKey, open.id)
    setEditing(store.add(scopeKey, { path, from, lines, comment: "" }))
    setCursor(hi)
  }
  const startRef = useRef(start)
  startRef.current = start

  useEffect(() => {
    const up = () => {
      if (!dragging.current) return
      dragging.current = false
      const s = selRef.current
      choose(null)
      if (s) startRef.current(...span(rows, s.anchor, s.focus))
    }
    window.addEventListener("mouseup", up)
    return () => window.removeEventListener("mouseup", up)
  }, [rows])
  // Indices into another diff (it refreshed) mean other lines.
  useEffect(() => {
    lastAnchor.current = null
    choose(null)
  }, [rows])
  // A block, not an arrow's value: where scrollIntoView returns a promise, React would take it for
  // the cleanup and throw on unmount.
  useEffect(() => {
    targetRow.current?.scrollIntoView({ block: "center" })
  }, [target, rows])

  const rowOf = (el: EventTarget | null): number | null => {
    const tr = el instanceof Element ? el.closest<HTMLElement>("tr[data-row]") : null
    return tr ? Number(tr.dataset.row) : null
  }

  const onMouseDown = (e: React.MouseEvent) => {
    const row = rowOf(e.target)
    if (e.button !== 0 || !(e.target as Element).closest("[data-gutter]") || !commentable(row)) return
    // No text selection, and the note being written keeps the focus until the new one opens.
    e.preventDefault()
    const anchor = e.shiftKey && commentable(lastAnchor.current) ? lastAnchor.current : row
    lastAnchor.current = anchor
    dragging.current = true
    choose({ anchor, focus: row })
  }
  const onMouseOver = (e: React.MouseEvent) => {
    const row = dragging.current ? rowOf(e.target) : null
    const s = selRef.current
    if (row !== null && s && s.focus !== row) choose({ ...s, focus: row })
  }
  const onKeyDown = (e: React.KeyboardEvent) => {
    const row = (e.target as Element).closest("[data-line]") ? rowOf(e.target) : null
    if (row === null) return
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault()
      const step = e.key === "ArrowDown" ? 1 : -1
      let next = row + step
      while (rows[next] && !takesNote(rows[next])) next += step
      if (!rows[next]) return
      if (e.shiftKey) {
        const anchor = selRef.current?.anchor ?? row
        const [lo, hi] = span(rows, anchor, next)
        next = step > 0 ? hi : lo
        if (next === row) return
        lastAnchor.current = anchor
        choose({ anchor, focus: next })
      } else choose(null)
      focusRow(next)
    } else if (e.key === "Escape" && selRef.current) {
      // Drops the lines being chosen; the viewer stays open.
      e.stopPropagation()
      choose(null)
    }
  }
  // Enter or Space on a number, or a screen reader's click (a mouse's is handled on press).
  const onClick = (e: React.MouseEvent) => {
    const row = e.detail === 0 && (e.target as Element).closest("[data-line]") ? rowOf(e.target) : null
    if (!commentable(row)) return
    const s = selRef.current
    choose(null)
    lastAnchor.current = s?.anchor ?? row
    start(...(s ? span(rows, s.anchor, s.focus) : ([row, row] as [number, number])))
  }
  const onFocus = (e: React.FocusEvent) => {
    const row = (e.target as Element).closest("[data-line]") ? rowOf(e.target) : null
    if (row !== null) setCursor(row)
  }

  const card = (n: Note, row: number | null) => (
    <NoteCard
      key={n.id}
      note={n}
      scopeKey={scopeKey}
      editing={editing === n.id}
      onEdit={() => setEditing(n.id)}
      onClose={() => {
        setEditing(null)
        if (row !== null) requestAnimationFrame(() => focusRow(row))
      }}
    />
  )
  const cols = layout === "diff" ? 4 : 2

  const body = (
    <table
      ref={table}
      className={cn(
        "w-full border-collapse",
        layout === "file" && "font-mono text-[12px] leading-[1.65]",
        // Hovering a line's number lights its numbers: the only sign they take a note.
        "[&_tr:has(>[data-gutter]:hover)>[data-gutter]]:bg-primary/10 [&_tr:has(>[data-gutter]:hover)>[data-gutter]]:text-foreground",
      )}
      onMouseDown={onMouseDown}
      onMouseOver={onMouseOver}
      onKeyDown={onKeyDown}
      onClick={onClick}
      onFocus={onFocus}
    >
      <tbody>
        {rows.map((l, i) => (
          <Fragment key={i}>
            <Row
              line={l}
              index={i}
              layout={layout}
              mark={range && i >= range[0] && i <= range[1] ? "chosen" : marks[i]}
              tabbable={i === tabRow}
              targetRef={layout === "file" && target === i + 1 ? targetRow : undefined}
            />
            {after.get(i)?.map((n) => (
              <tr key={n.id}>
                <td colSpan={cols} className="py-1">
                  {card(n, i)}
                </td>
              </tr>
            ))}
          </Fragment>
        ))}
      </tbody>
    </table>
  )

  return (
    <div className={cn("@container", layout === "diff" && "overflow-auto font-mono text-[12px] leading-[1.65]", className)}>
      {lost.length ? (
        <div className="sticky left-0 flex flex-col gap-1 px-2 pb-1">
          <div className="pl-1 font-sans text-[11px] text-faint">{lost.length === 1 ? "Note for lines that changed:" : "Notes for lines that changed:"}</div>
          {lost.map((n) => card(n, null))}
        </div>
      ) : null}
      {body}
    </div>
  )
}

// Gold behind the numbers, never on them: gold text is unreadable on the light theme's tints.
const GUTTER_MARK = { chosen: "bg-primary/20 text-foreground", noted: "bg-primary/10 text-foreground/80", first: "bg-primary/10 text-foreground/80" }

/** One line. Memoized: choosing lines across a long file redraws only the lines that change. */
const Row = memo(function Row({ line: l, index, layout, mark, tabbable, targetRef }: {
  line: ViewLine
  index: number
  layout: "diff" | "file"
  mark?: "chosen" | "noted" | "first"
  tabbable: boolean
  targetRef?: React.RefObject<HTMLTableRowElement | null>
}) {
  const takes = takesNote(l)
  const removed = l.cls === "d"
  const n = removed ? l.old : (l.new ?? l.old)
  // The number to press (the new one, or the old one on a removed line): one tab stop for all of
  // them, arrows move between lines.
  const button =
    takes && n !== undefined ? (
      <button
        type="button"
        data-line
        tabIndex={tabbable ? 0 : -1}
        aria-label={`Add a note to ${removed ? "removed " : ""}line ${n}`}
        className="cursor-pointer rounded-sm focus-visible:outline-1 focus-visible:outline-offset-1"
      >
        {n}
      </button>
    ) : null
  // The numbers' cells: where a press starts choosing lines, tinted while chosen or noted.
  const gutter = takes ? { "data-gutter": "" } : {}
  const tint = cn(takes && "cursor-pointer", takes && mark && GUTTER_MARK[mark])
  const bar = mark === "first" && "shadow-[inset_2px_0_0_var(--primary)]"
  const chosen = mark === "chosen" && "bg-primary/10"

  if (layout === "file")
    return (
      <tr data-row={index} ref={targetRef} className={targetRef ? "bg-primary/15" : undefined}>
        <td {...gutter} className={cn("w-12 pr-3 text-right align-top text-faint select-none", tint, bar)}>
          {button ?? index + 1}
        </td>
        <td className={cn("pr-4 whitespace-pre text-foreground/90 select-text", chosen)}>{l.text || " "}</td>
      </tr>
    )

  const cell = "w-10 pr-2 text-right align-top whitespace-nowrap text-faint select-none"
  return (
    <tr data-row={index} className={l.cls === "a" ? "bg-add" : l.cls === "d" ? "bg-del" : undefined}>
      {/* Never broken: in a narrow pane the code scrolls sideways, the numbers stay whole. */}
      <td {...gutter} className={cn(cell, tint, bar)}>
        {l.cls === "h" ? "" : removed && button ? button : (l.old ?? "")}
      </td>
      <td {...gutter} className={cn(cell, tint)}>
        {l.cls === "h" ? "" : !removed && button ? button : (l.new ?? "")}
      </td>
      <td className={cn("w-4 align-top select-none", l.cls === "a" ? "text-green" : l.cls === "d" ? "text-red" : "text-faint", chosen)}>
        {l.cls === "a" ? "+" : l.cls === "d" ? "−" : ""}
      </td>
      <td className={cn("pr-4 whitespace-pre select-text", l.cls === "h" ? "py-0.5 text-cyan/80" : l.cls === "a" ? "text-green" : l.cls === "d" ? "text-red" : "text-muted-foreground", chosen)}>
        {l.text || " "}
      </td>
    </tr>
  )
})

/** A note under its lines: what it says, or a field to write it in. */
function NoteCard({ note, scopeKey, editing, onEdit, onClose }: { note: Note; scopeKey: string; editing: boolean; onEdit: () => void; onClose: () => void }) {
  const comment = useNotes((s) => s.byScope[scopeKey]?.find((n) => n.id === note.id)?.comment ?? "")
  const area = useRef<HTMLTextAreaElement>(null)
  // What Esc goes back to: nothing, for a new note (it goes).
  const before = useRef(comment)
  useEffect(() => {
    if (!editing) return
    before.current = comment
    const el = area.current
    el?.focus({ preventScroll: true })
    el?.setSelectionRange(el.value.length, el.value.length)
    el?.scrollIntoView({ block: "nearest" })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing])
  const store = useNotes.getState
  const label = lineLabel(note)
  const save = () => {
    if (!comment.trim()) store().remove(scopeKey, note.id)
    onClose()
  }
  const cancel = () => {
    if (before.current.trim()) store().write(scopeKey, note.id, before.current)
    else store().remove(scopeKey, note.id)
    onClose()
  }

  return (
    <div className="sticky left-2 w-[min(40rem,calc(100cqi-1rem))] rounded-lg border bg-card px-2.5 py-1.5 font-sans text-[12.5px] leading-normal whitespace-normal text-card-foreground shadow-sm">
      <div className="flex h-5 items-center gap-1.5 text-[11px] text-faint">
        <MessageSquare className="size-3 text-primary" />
        <span>{label}</span>
        {!editing ? (
          <span className="ml-auto flex">
            <CardButton label="Edit note" onClick={onEdit}>
              <Pencil />
            </CardButton>
            <CardButton label="Remove note" onClick={() => store().remove(scopeKey, note.id)}>
              <X />
            </CardButton>
          </span>
        ) : null}
      </div>
      {editing ? (
        <>
          {/* Sized, and contained: typing in it lays out this box, not the table of a long
              diff around it (every row, on each key, in Chromium). */}
          <div className="mt-1 [contain:strict]" style={{ height: Math.min(8, Math.max(2, comment.split("\n").length)) * 18 + 14 }}>
            <textarea
              ref={area}
              value={comment}
              onChange={(e) => store().write(scopeKey, note.id, e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault()
                  save()
                } else if (e.key === "Escape") {
                  e.preventDefault()
                  e.stopPropagation()
                  cancel()
                }
              }}
              placeholder="A note for the agent…"
              aria-label={`Note on ${label.toLowerCase()}`}
              className="rounded-md block size-full resize-none border border-input bg-background px-2 py-1.5 text-[12.5px] leading-[18px] outline-none placeholder:text-faint focus:border-ring/70"
            />
          </div>
          <div className="mt-1.5 flex items-center gap-1.5">
            <span className="min-w-0 truncate text-[11px] text-faint">Enter to save, Shift+Enter for a new line</span>
            <span className="ml-auto flex shrink-0 gap-1">
              <button type="button" className="rounded-md h-6 px-2 text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground" onClick={cancel}>
                Cancel
              </button>
              <button
                type="button"
                className="rounded-md h-6 bg-primary px-2.5 text-[12px] font-medium text-primary-foreground hover:bg-primary/90 disabled:bg-muted disabled:text-faint"
                disabled={!comment.trim()}
                onClick={save}
              >
                {before.current.trim() ? "Save" : "Add note"}
              </button>
            </span>
          </div>
        </>
      ) : (
        <div className="cursor-text break-words whitespace-pre-wrap select-text" onDoubleClick={onEdit}>
          {comment}
        </div>
      )}
    </div>
  )
}

function CardButton({ label, onClick, children }: { label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <Tip content={label}>
      <button type="button" className="rounded-md grid size-5 place-items-center text-faint hover:bg-accent hover:text-foreground [&_svg]:size-3" aria-label={label} onClick={onClick}>
        {children}
      </button>
    </Tip>
  )
}
