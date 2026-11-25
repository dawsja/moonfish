import { useEffect, useState } from "react"
import { isImagePath } from "@/lib/git"
import { ReadImage } from "./ChatImages"
import { Brain, Check, ChevronDown, ChevronRight, FilePen, FileText, Globe, ListTree, Loader2, Search, SquareTerminal, Wrench, X, type LucideIcon } from "lucide-react"
import type { Item, ToolItem, ToolKind } from "@/lib/chat"
import { cn } from "@/lib/utils"
import { useRelative } from "./cwd"
import { useTouch } from "./touch"
import { DiffCard, DiffStat } from "./DiffView"

const ICON: Record<ToolKind, LucideIcon> = {
  read: FileText,
  edit: FilePen,
  write: FilePen,
  bash: SquareTerminal,
  search: Search,
  web: Globe,
  task: ListTree,
  other: Wrench,
}

const VERB: Record<ToolKind, string> = {
  read: "Read",
  edit: "Edit",
  write: "Write",
  bash: "Run",
  search: "Search",
  web: "Fetch",
  task: "Task",
  other: "Tool",
}

/** A run of reasoning and tool calls: one summary line, expanded while the turn runs. Edits
 *  keep their diffs visible even when the run is folded. */
/** A Read of an image file that worked: shown as a thumbnail. */
const viewedImage = (t: ToolItem) => t.tool.kind === "read" && t.status === "ok" && !!t.tool.path && isImagePath(t.tool.path)

export function WorkGroup({ items, live, jumpTarget }: { items: Item[]; live: boolean; jumpTarget?: { id: string; seq: number } | null }) {
  const [open, setOpen] = useState(live)
  const touch = useTouch()
  // Open while the agent works on it, folded once the turn moves on.
  useEffect(() => setOpen(live), [live])
  const hasJumpTarget = !!jumpTarget && items.some((it) => it.id === jumpTarget.id)
  useEffect(() => {
    if (hasJumpTarget) setOpen(true)
  }, [hasJumpTarget, jumpTarget?.seq])
  const tools = items.filter((i): i is ToolItem => i.kind === "tool")
  const running = tools.some((t) => t.status === "running")
  const failed = tools.filter((t) => t.status === "error").length
  const names = [...new Set(tools.map((t) => VERB[t.tool.kind]))]
  const diffs = tools.filter((t) => t.diff)
  const viewed = tools.filter(viewedImage)
  const summary = tools.length === 0 ? "Thought" : `${tools.length} step${tools.length === 1 ? "" : "s"}`

  return (
    <div className="flex flex-col gap-1.5">
      <button className={cn("rounded-md group flex items-center gap-1.5 self-start text-muted-foreground hover:text-foreground", touch ? "h-10 pr-2 text-[13.5px]" : "h-6 text-[12.5px]")} onClick={() => setOpen(!open)}>
        {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
        <span className={cn(running && "shimmer")}>{summary}</span>
        {names.length ? <span className="text-faint">· {names.join(", ")}</span> : null}
        {failed ? <span className="text-red">· {failed} failed</span> : null}
      </button>
      {open ? (
        <div className="ml-[7px] flex flex-col gap-0.5 border-l pl-4">
          {items.map((it) => (it.kind === "reasoning" ? <Reasoning key={it.id} text={it.text} /> : it.kind === "tool" ? <ToolRow key={it.id} item={it} /> : null))}
        </div>
      ) : (
        <>
          {/* Images the agent looked at stay visible with the steps folded. */}
          {viewed.map((t) => (
            <ReadImage key={t.id} path={t.tool.path!} />
          ))}
          {diffs.map((t) => (
            <DiffCard key={t.id} diff={t.diff!} defaultOpen={false} />
          ))}
        </>
      )}
    </div>
  )
}

function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false)
  const touch = useTouch()
  return (
    <div className="py-0.5">
      <button className={cn("flex w-full items-start gap-2 text-left text-faint hover:text-muted-foreground", touch ? "min-h-10 py-2.5 text-[13.5px]" : "min-h-6 text-[12.5px]")} onClick={() => setOpen(!open)}>
        <Brain className="mt-[3px] size-3.5 shrink-0" />
        <span className={cn("italic", !open && "line-clamp-1")}>{text}</span>
      </button>
    </div>
  )
}

function formatToolInput(input: unknown): string | null {
  if (input == null) return null
  if (typeof input === "string") return input.trim() || null
  if (Array.isArray(input)) return input.length ? JSON.stringify(input, null, 2) : null
  if (typeof input === "object") {
    if (!Object.keys(input).length) return null
    try {
      return JSON.stringify(input, null, 2)
    } catch {
      return String(input)
    }
  }
  return String(input)
}

export function ToolRow({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false)
  const rel = useRelative()
  const touch = useTouch()
  const Icon = ICON[item.tool.kind]
  const input = formatToolInput(item.tool.input)
  const hasInput = !!input
  const hasOutput = !!item.output.trim()
  const expandable = !item.diff && (hasInput || hasOutput)
  return (
    <div data-item-id={item.id} className="rounded-md py-0.5">
      <button
        className={cn("flex w-full items-center gap-2 text-left hover:text-foreground disabled:cursor-default", touch ? "min-h-10 text-[13.5px]" : "min-h-6 text-[12.5px]")}
        onClick={() => setOpen(!open)}
        disabled={!hasInput && !hasOutput && !item.diff}
        aria-expanded={expandable ? open : undefined}
      >
        <Icon className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="shrink-0 text-muted-foreground">{VERB[item.tool.kind]}</span>
        <span className={cn("min-w-0 truncate font-mono text-[12px]", item.tool.kind === "bash" ? "text-code" : "text-foreground")}>{rel(item.tool.title)}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2 pl-2">
          {item.diff ? <DiffStat added={item.diff.added} removed={item.diff.removed} /> : null}
          {item.status === "running" ? (
            <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
          ) : item.status === "ok" ? (
            <Check className="size-3.5 text-green" />
          ) : (
            <X className="size-3.5 text-red" />
          )}
        </span>
      </button>
      {viewedImage(item) ? <ReadImage path={item.tool.path!} /> : null}
      {item.diff ? (
        <div className="mt-1 mb-1.5">
          <DiffCard diff={item.diff} defaultOpen />
        </div>
      ) : open && (hasInput || hasOutput) ? (
        <div className="mt-1 mb-1.5 flex flex-col gap-1.5">
          {hasInput ? (
            <div className="rounded-lg overflow-hidden border bg-card">
              <div className="border-b bg-muted/40 px-3 py-1 text-[10.5px] font-medium text-faint">Input</div>
              <pre className="max-h-48 overflow-auto px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap text-muted-foreground select-text">
                {input}
              </pre>
            </div>
          ) : null}
          {hasOutput ? (
            <div className="rounded-lg overflow-hidden border bg-card">
              {hasInput ? <div className="border-b bg-muted/40 px-3 py-1 text-[10.5px] font-medium text-faint">Output</div> : null}
              <pre className={cn("max-h-64 overflow-auto px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap select-text", item.status === "error" ? "text-red" : "text-muted-foreground")}>
                {item.output}
              </pre>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}
