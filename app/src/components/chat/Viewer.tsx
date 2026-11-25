import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeft, AtSign, MessageSquareShare, Minus, Plus } from "lucide-react"
import { git, isImagePath, type Scope } from "@/lib/git"
import type { NoteFrom, ViewLine } from "@/lib/notes"
import { ImageStage } from "@/components/ImageView"
import { useApp } from "@/store/app"
import { key, useGit, type Viewer as V } from "@/store/git"
import { insertNotes } from "@/store/insert"
import { useNoteCount } from "@/store/notes"
import { addToChat } from "@/components/right/Files"
import { DiffStat, parsePatch } from "./DiffView"
import { NoteLines } from "./LineNotes"

/** A diff or a file from the side panel, read-only, in place of the chat. Its line numbers take
 *  notes for the agent, sent to the chat's composer together. */
export function Viewer({ scope, viewer }: { scope: Scope; viewer: V }) {
  const open = useCallback((v: V | null) => useApp.getState().setActive({ viewer: v }), [])
  const run = useGit((s) => s.run)
  const [text, setText] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  // A diff follows the working tree, but only this file's part of it: a refresh that changed
  // nothing here (every few seconds) doesn't reload it or jump back to the top.
  const fileState = useGit((s) => {
    if (viewer.kind !== "diff") return ""
    const st = s.status[`${scope.projectId}|${scope.cwd ?? ""}`]
    if (!st) return ""
    const all = [...st.staged, ...st.unstaged, ...st.untracked, ...st.conflicted]
    return JSON.stringify(all.filter((c) => c.path === viewer.path))
  })
  const shown = useRef("")

  useEffect(() => {
    const what = JSON.stringify([viewer, scope.projectId, scope.cwd])
    // Another file: "Loading…" until it's read. The same one again: the old text stays meanwhile.
    if (shown.current !== what) {
      setText(null)
      setError(null)
    }
    shown.current = what
    let current = true
    const p =
      viewer.kind === "diff"
        ? git.diff(scope, viewer.path, viewer.staged, viewer.untracked)
        : viewer.kind === "commit"
          ? git.commitDiff(scope, viewer.hash, viewer.path)
          : isImagePath(viewer.path)
            ? git.readImage(scope, viewer.path)
            : git.read(scope, viewer.path)
    // A slow answer for a file clicked before this one never lands under this one's name.
    p.then(
      (t) => current && setText(t),
      (e) => current && setError(String(e)),
    )
    return () => void (current = false)
  }, [viewer, scope.projectId, scope.cwd, fileState])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !(e.target instanceof HTMLTextAreaElement) && open(null)
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [open])

  const lines = useMemo<ViewLine[]>(() => {
    if (text === null || (viewer.kind === "file" && isImagePath(viewer.path))) return []
    if (viewer.kind !== "file") return parsePatch(text)
    return text.replace(/\n$/, "").split("\n").map((t, i) => ({ cls: "c", text: t, new: i + 1 }))
  }, [text, viewer.kind, viewer.path])
  const added = viewer.kind === "file" ? 0 : lines.filter((l) => l.cls === "a").length
  const removed = viewer.kind === "file" ? 0 : lines.filter((l) => l.cls === "d").length
  const scopeKey = key(scope)
  const notes = useNoteCount(scopeKey)
  const from: NoteFrom = useMemo(
    () => (viewer.kind === "diff" ? { kind: "diff", staged: viewer.staged } : viewer.kind === "commit" ? { kind: "commit", hash: viewer.hash } : { kind: "file" }),
    [viewer],
  )

  return (
    <div className="flex h-full min-w-0 flex-col bg-background">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
        <button className="rounded-md flex h-7 items-center gap-1.5 px-2 text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => open(null)}>
          <ArrowLeft className="size-3.5" /> Chat
        </button>
        <span className="h-4 w-px bg-border" />
        <span className="truncate font-mono text-[12.5px]">{viewer.path}</span>
        {viewer.kind === "diff" ? <span className="rounded-full border px-1.5 text-[10.5px] text-muted-foreground">{viewer.untracked ? "new file" : viewer.staged ? "staged" : "working tree"}</span> : null}
        {viewer.kind === "commit" ? <span className="truncate text-[11.5px] text-faint">{viewer.hash.slice(0, 8)} · {viewer.subject}</span> : null}
        <span className="ml-auto flex items-center gap-1">
          {viewer.kind !== "file" && text ? <DiffStat added={added} removed={removed} /> : null}
          {viewer.kind === "diff" ? (
            viewer.staged ? (
              <ActionButton onClick={() => run(scope, "Unstaging…", () => git.unstage(scope, [viewer.path])).then(() => open({ ...viewer, staged: false }))}>
                <Minus /> Unstage
              </ActionButton>
            ) : (
              <ActionButton onClick={() => run(scope, "Staging…", () => git.stage(scope, [viewer.path])).then(() => open({ ...viewer, staged: true, untracked: false }))}>
                <Plus /> Stage
              </ActionButton>
            )
          ) : null}
          <ActionButton onClick={() => addToChat(viewer.path)}>
            <AtSign /> Add to chat
          </ActionButton>
        </span>
      </div>
      <div className="relative flex min-h-0 flex-1 flex-col">
        <div className="min-h-0 flex-1 overflow-auto">
          {error ? <div className="rounded-lg m-4 border border-red/40 bg-del/40 p-3 text-[12.5px] text-red">{error}</div> : null}
          {text === null && !error ? <div className="p-4 text-[12px] text-faint">Loading…</div> : null}
          {text !== null && viewer.kind !== "file" ? text.trim() ? <NoteLines rows={lines} layout="diff" scopeKey={scopeKey} path={viewer.path} from={from} className={notes ? "pt-2 pb-12" : "py-2"} /> : <div className="p-4 text-[12px] text-faint">No differences.</div> : null}
          {text !== null && viewer.kind === "file" ? (
            isImagePath(viewer.path) ? (
              <ImageStage image={{ src: text, name: viewer.path.split("/").pop() ?? viewer.path }} className="h-full" />
            ) : (
              <NoteLines rows={lines} layout="file" scopeKey={scopeKey} path={viewer.path} from={from} target={viewer.line} className={notes ? "pb-12" : undefined} />
            )
          ) : null}
        </div>
        {notes ? (
          // Every file's notes in this workspace go, not only this one's.
          <button
            className="rounded-md absolute right-4 bottom-4 z-10 flex h-7 items-center gap-1.5 border border-input bg-popover px-2.5 text-[12px] font-medium text-popover-foreground shadow-md hover:bg-accent"
            onClick={() => insertNotes(scopeKey)}
          >
            <MessageSquareShare className="size-3.5 text-primary" />
            {notes === 1 ? "Add the note to chat" : `Add ${notes} notes to chat`}
          </button>
        ) : null}
      </div>
    </div>
  )
}

function ActionButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button className="rounded-md flex h-7 items-center gap-1.5 border border-input px-2.5 text-[12px] hover:bg-accent [&_svg]:size-3.5" onClick={onClick}>
      {children}
    </button>
  )
}
