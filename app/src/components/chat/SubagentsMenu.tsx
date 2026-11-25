import { useEffect, useState } from "react"
import { ArrowUpRight, Check, ChevronDown, ChevronRight, CircleDashed, Copy, MessageSquare, X } from "lucide-react"
import { Tip } from "@/components/Tip"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { formatDuration } from "@/lib/chat"
import { copyText } from "@/lib/clipboard"
import { subagentSummary, type SubagentItem } from "@/lib/subagents"
import { cn } from "@/lib/utils"

const MAX_STEPS = 5

/** The current time, ticking every second while `on`. */
function useNow(on: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!on) return
    setNow(Date.now())
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [on])
  return now
}

/** Tracks the most recent distinct live step strings per subagent while the chat is mounted. */
export function useSubagentSteps(subagents: SubagentItem[]): Record<string, string[]> {
  const [history, setHistory] = useState<Record<string, string[]>>({})
  useEffect(() => {
    setHistory((prev) => {
      let changed = false
      const next = { ...prev }
      for (const s of subagents) {
        const step = s.detail?.trim()
        if (!step) continue
        const list = prev[s.id] ?? []
        if (list[list.length - 1] !== step) {
          next[s.id] = [...list.slice(-(MAX_STEPS - 1)), step]
          changed = true
        }
      }
      return changed ? next : prev
    })
  }, [subagents])
  return history
}

function firstNonEmptyLine(text: string | undefined): string | undefined {
  if (!text) return undefined
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (trimmed) return trimmed
  }
  return undefined
}

function elapsedLabel(item: SubagentItem, now: number): string | null {
  if (item.status === "running") {
    if (item.startedAt === undefined) return null
    return formatDuration(Math.max(0, now - item.startedAt))
  }
  const word = item.status === "completed" ? "done" : item.status
  if (item.startedAt !== undefined && item.endedAt !== undefined) {
    return `${word} · ${formatDuration(Math.max(0, item.endedAt - item.startedAt))}`
  }
  return word
}

export function SubagentsMenu({
  subagents,
  onStop,
  onOpenSubagent,
  onJumpToItem,
}: {
  subagents: SubagentItem[]
  onStop?: (taskId: string) => void
  onOpenSubagent?: (subagent: SubagentItem) => void
  onJumpToItem?: (itemId: string) => void
}) {
  const [open, setOpen] = useState(false)
  const running = subagents.filter((s) => s.status === "running")
  const stepsById = useSubagentSteps(running)

  useEffect(() => {
    if (!running.length && open) setOpen(false)
  }, [running.length, open])

  if (!running.length) return null

  const count = running.length
  const label = `${count} ${count === 1 ? "subagent" : "subagents"}`
  const summary = subagentSummary(running)

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="flex h-7 shrink-0 items-center gap-1.5 rounded-full border bg-card px-2.5 text-[12px] font-medium text-foreground transition-colors hover:bg-accent"
          aria-label={`Subagents: ${summary}`}
        >
          <span className="size-2 shrink-0 rounded-full bg-green animate-pulse" />
          <span>{label}</span>
          <ChevronDown className={cn("size-3.5 text-muted-foreground transition-transform", open && "rotate-180")} />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" sideOffset={6} className="w-[380px] max-w-[calc(100vw-2rem)] overflow-hidden rounded-xl border bg-popover p-0 shadow-xl">
        <div className="flex items-center gap-1.5 border-b px-3 py-2.5">
          <span className="text-[12.5px] font-semibold text-foreground">Running subagents</span>
          {summary ? <span className="text-faint">·</span> : null}
          {summary ? <span className="truncate font-mono text-[11px] text-faint">{summary}</span> : null}
        </div>
        <SubagentsPanel
          subagents={running}
          open={open}
          stepsById={stepsById}
          onStop={onStop}
          onOpenSubagent={onOpenSubagent}
          onJumpToItem={onJumpToItem}
          onClose={() => setOpen(false)}
        />
      </PopoverContent>
    </Popover>
  )
}

export function SubagentsPanel({
  subagents,
  open,
  stepsById,
  onStop,
  onOpenSubagent,
  onJumpToItem,
  onClose,
  touch = false,
}: {
  subagents: SubagentItem[]
  open: boolean
  stepsById: Record<string, string[]>
  onStop?: (taskId: string) => void
  onOpenSubagent?: (subagent: SubagentItem) => void
  onJumpToItem?: (itemId: string) => void
  onClose?: () => void
  touch?: boolean
}) {
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const running = subagents.filter((s) => s.status === "running")
  const now = useNow(open && running.length > 0)

  useEffect(() => {
    if (!open || onOpenSubagent) return
    setExpandedId((current) => {
      if (current && running.some((s) => s.id === current)) return current
      return running[0]?.id ?? null
    })
  }, [open, running, onOpenSubagent])

  return (
    <div className="flex min-h-0 flex-col">
      <div className={cn("flex flex-col gap-1.5 overflow-y-auto overscroll-contain", touch ? "max-h-[65dvh] px-3 py-2.5" : "max-h-[min(70vh,440px)] p-2")}>
        {running.length === 0 ? (
          <div className="px-2 py-4 text-center text-[12px] text-faint">No running subagents.</div>
        ) : (
          running.map((item) => (
            <SubagentRow
              key={item.id}
              item={item}
              steps={stepsById[item.id] ?? []}
              expanded={!onOpenSubagent && expandedId === item.id}
              now={now}
              touch={touch}
              onOpen={
                onOpenSubagent
                  ? () => {
                      onOpenSubagent(item)
                      onClose?.()
                    }
                  : undefined
              }
              onToggle={() => setExpandedId((cur) => (cur === item.id ? null : item.id))}
              onStop={item.taskId && onStop ? () => onStop(item.taskId!) : undefined}
              onJump={
                item.itemId && onJumpToItem
                  ? () => {
                      onJumpToItem(item.itemId!)
                      onClose?.()
                    }
                  : undefined
              }
            />
          ))
        )}
      </div>
    </div>
  )
}

function SubagentRow({
  item,
  steps,
  expanded,
  now,
  touch,
  onOpen,
  onToggle,
  onStop,
  onJump,
}: {
  item: SubagentItem
  steps: string[]
  expanded: boolean
  now: number
  touch: boolean
  onOpen?: () => void
  onToggle: () => void
  onStop?: () => void
  onJump?: () => void
}) {
  const [copied, setCopied] = useState(false)
  const live = item.status === "running"
  const canStop = live && item.stoppable && !!onStop
  const elapsed = elapsedLabel(item, now)
  const subline = item.detail ?? firstNonEmptyLine(item.prompt)
  const showRole = !!item.role && item.role.toLowerCase() !== item.title.toLowerCase()
  const activeStep = steps[steps.length - 1] ?? item.detail
  const priorSteps = steps.length > 1 ? steps.slice(0, -1) : []
  const showDetailInExpanded = !!activeStep
  const hasBody = !!item.prompt || showDetailInExpanded || !!item.output

  return (
    <div
      className={cn(
        "overflow-hidden rounded-lg border transition-colors",
        expanded ? "border-border bg-card" : "border-border/60 bg-card/50 hover:bg-accent/60",
      )}
    >
      <div className={cn("flex items-center gap-2", touch ? "px-3 py-2.5" : "px-2.5 py-2")}>
        <button
          type="button"
          className="flex min-w-0 flex-1 flex-col gap-0.5 text-left"
          aria-expanded={onOpen ? undefined : expanded}
          onClick={onOpen ?? onToggle}
        >
          <div className="flex w-full items-center gap-2">
            <StatusDot status={item.status} tracked={item.tracked} />
            <Tip content={!touch && item.title.length > 34 ? item.title : null}>
              <span className={cn("min-w-0 flex-1 truncate font-medium text-foreground", touch ? "text-[13.5px]" : "text-[12.5px]")}>
                {item.title}
              </span>
            </Tip>
            {showRole ? (
              <span className="max-w-24 shrink-0 truncate rounded-full border bg-muted/60 px-1.5 py-px font-mono text-[10px] text-muted-foreground">
                {item.role}
              </span>
            ) : null}
            {elapsed ? <span className="shrink-0 font-mono text-[11px] text-faint tabular-nums">{elapsed}</span> : null}
            {onOpen ? (
              <span className="flex shrink-0 items-center gap-0.5 text-[11px] text-muted-foreground">
                {item.interactive ? <MessageSquare className="size-3" /> : null}
                <ChevronRight className="size-3.5" />
              </span>
            ) : null}
          </div>
          {!expanded && subline ? (
            <div className={cn("truncate pl-4 text-muted-foreground", touch ? "text-[12.5px]" : "text-[11.5px]")}>
              {subline}
            </div>
          ) : null}
        </button>

        {canStop && !expanded ? (
          <Tip content={!touch ? "Stop" : null}>
            <button
              type="button"
              className={cn(
                "grid shrink-0 place-items-center rounded-md text-faint transition-colors hover:bg-del/50 hover:text-red",
                touch ? "size-7" : "size-5",
              )}
              aria-label={`Stop ${item.title}`}
              onClick={(e) => {
                e.stopPropagation()
                onStop?.()
              }}
            >
              <X className={touch ? "size-3.5" : "size-3"} />
            </button>
          </Tip>
        ) : null}
      </div>

      {expanded ? (
        <div className={cn("flex flex-col gap-2 border-t border-border/60 bg-background/40", touch ? "px-3 py-2.5 text-[13px]" : "px-3 py-2.5 text-[12px]")}>
          {item.prompt ? (
            <div className="flex flex-col gap-0.5">
              <div className="text-[10.5px] font-medium text-faint">Prompt</div>
              <div className="max-h-24 overflow-y-auto leading-relaxed whitespace-pre-wrap break-words text-muted-foreground select-text">
                {item.prompt}
              </div>
            </div>
          ) : null}

          {showDetailInExpanded ? (
            <div className="flex flex-col gap-1">
              <div className="text-[10.5px] font-medium text-faint">Live step</div>
              {priorSteps.length > 0 ? (
                <ul className="flex flex-col gap-0.5">
                  {priorSteps.map((step, idx) => (
                    <li key={`${idx}-${step}`} className="flex items-start gap-1.5 text-[11.5px] text-faint">
                      <Check className="mt-0.5 size-3 shrink-0 text-green/70" />
                      <span className="min-w-0 break-words">{step}</span>
                    </li>
                  ))}
                </ul>
              ) : null}
              <div className="leading-snug break-words text-foreground select-text">{activeStep}</div>
            </div>
          ) : null}

          {item.output ? (
            <div className="flex flex-col gap-1">
              <div className="flex items-center justify-between">
                <span className="text-[10.5px] font-medium text-faint">Output</span>
                <button
                  type="button"
                  className={cn(
                    "inline-flex items-center gap-1 rounded px-1.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                    touch ? "h-7" : "h-5",
                    copied && "text-foreground",
                  )}
                  onClick={() => {
                    copyText(item.output!)
                    setCopied(true)
                    setTimeout(() => setCopied(false), 1200)
                  }}
                >
                  {copied ? <Check className="size-3 text-green" /> : <Copy className="size-3" />}
                  <span>{copied ? "Copied" : "Copy"}</span>
                </button>
              </div>
              <pre className="max-h-36 overflow-auto rounded-md border bg-background/80 px-2.5 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-words text-muted-foreground select-text">
                {item.output}
              </pre>
            </div>
          ) : null}

          {!hasBody ? <div className="text-[11.5px] text-faint">Waiting for updates...</div> : null}

          {onJump || canStop ? (
            <div className="flex items-center justify-between gap-2 pt-0.5">
              {onJump ? (
                <button
                  type="button"
                  className={cn(
                    "inline-flex items-center gap-1 font-medium text-cyan hover:underline",
                    touch ? "h-8 text-[13px]" : "text-[11.5px]",
                  )}
                  onClick={onJump}
                >
                  <span>Jump to chat</span>
                  <ArrowUpRight className={touch ? "size-3.5" : "size-3"} />
                </button>
              ) : (
                <span />
              )}
              {canStop ? (
                <button
                  type="button"
                  className={cn(
                    "inline-flex items-center gap-1 rounded-md border border-red/30 bg-del/50 px-2 font-medium text-red hover:bg-del",
                    touch ? "h-8 text-[12.5px]" : "h-6 text-[11px]",
                  )}
                  onClick={onStop}
                >
                  <X className="size-3" />
                  <span>Stop</span>
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}

function StatusDot({ status, tracked }: { status: SubagentItem["status"]; tracked: boolean }) {
  if (status === "running") {
    return tracked ? (
      <span className="size-2 shrink-0 rounded-full bg-green animate-pulse" aria-label="Running" />
    ) : (
      <CircleDashed className="size-3.5 shrink-0 text-muted-foreground" aria-label="Started" />
    )
  }
  return <Check className="size-3.5 shrink-0 text-green" aria-label="Completed" />
}
