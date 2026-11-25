import { useEffect, useState } from "react"
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  CloudDownload,
  FileDiff,
  Files as FilesIcon,
  FileText,
  GitBranch,
  GitPullRequest,
  History as HistoryIcon,
  LoaderCircle,
  MoreHorizontal,
  RefreshCw,
  Trash2,
  X,
  type LucideIcon,
} from "lucide-react"
import { promptText } from "@/components/Prompt"
import { Tip } from "@/components/Tip"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { SubagentsPanel, useSubagentSteps } from "@/components/chat/SubagentsMenu"
import { formatDuration, type ChatMeta } from "@/lib/chat"
import { copyText } from "@/lib/clipboard"
import { git, type Scope, type Status } from "@/lib/git"
import type { SubagentItem } from "@/lib/subagents"
import { cn } from "@/lib/utils"
import { useApp, useProject } from "@/store/app"
import { copyChatMarkdown, useChats } from "@/store/chats"
import { useGit, useStatus } from "@/store/git"
import { BranchPicker } from "./BranchPicker"
import { Files } from "./Files"
import { History } from "./History"
import { PullRequest } from "./PullRequest"
import { changeTotals, SourceControl } from "./SourceControl"

export type ControlCenterProps = {
  scope: Scope | null
  meta?: ChatMeta
  running?: boolean
  subagents?: SubagentItem[]
  onStopTask?: (taskId: string) => void
  onOpenSubagent?: (subagent: SubagentItem) => void
  onJumpToItem?: (itemId: string) => void
  belowToolbar?: boolean
}

type ExpandedSection = null | "subagents" | "scm" | "files" | "history" | "pr"

const WORKSPACE_TOOLS: { id: "files" | "history" | "pr"; label: string; icon: LucideIcon }[] = [
  { id: "files", label: "Files", icon: FilesIcon },
  { id: "history", label: "History", icon: HistoryIcon },
  { id: "pr", label: "PR", icon: GitPullRequest },
]

/** Solid Composer-matched surface tokens shared by the open card and collapsed corner pill. */
const SURFACE =
  "border border-border bg-card shadow-[0_6px_20px_-12px_rgb(15_23_42/0.25)] dark:shadow-[0_1px_0_rgb(255_255_255/0.03)_inset,0_8px_24px_-12px_rgb(0_0_0/0.6)]"

/** Ticks every second while live subagents are displayed so resting elapsed timers stay fresh. */
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

/** Floating solid top-right Control Center: branch/sync, session actions, live subagents,
 *  and inline expandable Source Control, Files, History, and Pull Request views. */
export function ControlCenter({
  scope,
  meta,
  running = false,
  subagents = [],
  onStopTask,
  onOpenSubagent,
  onJumpToItem,
  belowToolbar = false,
}: ControlCenterProps) {
  const layout = useApp((s) => s.layout)
  const setLayout = useApp((s) => s.setLayout)
  const project = useProject()
  const { status, loading: statusLoading } = useStatus(scope)
  const [expanded, setExpanded] = useState<ExpandedSection>(null)

  const runningSubagents = subagents.filter((s) => s.status === "running")
  const finishedSubagents = subagents.filter((s) => s.status !== "running")
  const stepsById = useSubagentSteps(runningSubagents)
  const now = useNow(layout.rightOpen && !layout.controlCollapsed && expanded === null && runningSubagents.length > 0)
  const totals = changeTotals(status)

  // Keep expanded state valid if workspace scope or subagents disappear.
  useEffect(() => {
    if (!scope && expanded && expanded !== "subagents") setExpanded(null)
  }, [scope, expanded])

  useEffect(() => {
    if (expanded === "subagents" && subagents.length === 0) setExpanded(null)
  }, [expanded, subagents.length])

  if (!layout.rightOpen || (!scope && !meta)) return null

  const posClass = belowToolbar ? "top-[52px]" : "top-3.5"
  const toggle = (section: Exclude<ExpandedSection, null>) => setExpanded((cur) => (cur === section ? null : section))

  // Collapsed corner pill state: quiet single-row summary in the top-right corner.
  if (layout.controlCollapsed) {
    return (
      <aside
        aria-label="Control center"
        className={cn(
          "absolute right-4 z-20 flex h-9 max-w-[calc(100%-2rem)] items-center gap-1 rounded-[14px] px-2 text-[12px]",
          posClass,
          SURFACE,
        )}
      >
        {running ? (
          <Tip content="Agent is working" side="bottom">
            <LoaderCircle className="ml-0.5 size-3.5 shrink-0 animate-spin text-muted-foreground" aria-label="Working" />
          </Tip>
        ) : null}

        {scope && status?.isRepo ? (
          <button
            type="button"
            className="flex h-7 min-w-0 items-center gap-1.5 rounded-md px-1.5 hover:bg-accent"
            aria-label="Expand control center"
            onClick={() => setLayout({ controlCollapsed: false })}
          >
            <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="max-w-28 truncate font-mono text-[12px] font-medium text-foreground">
              {status.branch ?? (status.head ? status.head.slice(0, 7) : "HEAD")}
            </span>
            {status.upstream && (status.behind > 0 || status.ahead > 0) ? (
              <span className="flex shrink-0 items-center gap-1 font-mono text-[11px] text-muted-foreground">
                {status.behind > 0 ? (
                  <span className="flex items-center text-cyan">
                    <ArrowDown className="size-3" />
                    {status.behind}
                  </span>
                ) : null}
                {status.ahead > 0 ? (
                  <span className="flex items-center text-foreground">
                    <ArrowUp className="size-3" />
                    {status.ahead}
                  </span>
                ) : null}
              </span>
            ) : null}
          </button>
        ) : project ? (
          <button
            type="button"
            className="flex h-7 min-w-0 items-center rounded-md px-1.5 text-[12px] font-medium text-foreground hover:bg-accent"
            onClick={() => setLayout({ controlCollapsed: false })}
          >
            <span className="max-w-28 truncate">{project.name}</span>
          </button>
        ) : null}

        {runningSubagents.length > 0 ? (
          <Tip content={`${runningSubagents.length} running ${runningSubagents.length === 1 ? "subagent" : "subagents"}`} side="bottom">
            <button
              type="button"
              className="flex h-6 shrink-0 items-center gap-1.5 rounded-full border border-green/30 bg-green/10 px-2 font-mono text-[10.5px] font-medium text-green hover:bg-green/15"
              onClick={() => {
                setExpanded("subagents")
                setLayout({ controlCollapsed: false })
              }}
            >
              <span className="size-1.5 shrink-0 rounded-full bg-green animate-pulse" />
              <span>{runningSubagents.length}</span>
            </button>
          </Tip>
        ) : null}

        {scope && totals.count > 0 ? (
          <Tip content={`${totals.count} uncommitted ${totals.count === 1 ? "change" : "changes"}`} side="bottom">
            <button
              type="button"
              className="flex h-6 shrink-0 items-center gap-1 rounded-full border border-border bg-secondary px-2 font-mono text-[10.5px] font-medium text-foreground hover:bg-accent"
              onClick={() => {
                setExpanded("scm")
                setLayout({ controlCollapsed: false })
              }}
            >
              <FileDiff className="size-3 text-muted-foreground" />
              <span>{totals.count > 99 ? "99" : totals.count}</span>
            </button>
          </Tip>
        ) : null}

        {meta ? (
          <>
            {(scope && status?.isRepo) || project || runningSubagents.length > 0 || (scope && totals.count > 0) ? (
              <span className="mx-0.5 h-3.5 w-px shrink-0 bg-border" aria-hidden="true" />
            ) : null}
            <SessionMenu meta={meta} />
          </>
        ) : null}

        <Tip content="Expand control center" side="bottom">
          <button
            type="button"
            className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="Expand control center"
            onClick={() => setLayout({ controlCollapsed: false })}
          >
            <ChevronDown className="size-3.5" />
          </button>
        </Tip>
      </aside>
    )
  }

  const workspaceExpanded = expanded === "files" || expanded === "history" || expanded === "pr"

  return (
    <aside
      aria-label="Control center"
      className={cn(
        "absolute right-4 z-20 flex max-w-[calc(100%-2rem)] flex-col overflow-hidden rounded-[18px] transition-[width] duration-200 ease-[cubic-bezier(0.22,1,0.36,1)]",
        posClass,
        belowToolbar ? "max-h-[calc(100%-5rem)]" : "max-h-[calc(100%-6rem)]",
        expanded ? "w-[320px]" : "w-[280px]",
        SURFACE,
      )}
    >
      {/* Top Row: Merged Branch + Ahead/Behind + Sync + Session Menu (⋯) + Collapse (▴) */}
      <div className="flex h-10 shrink-0 items-center gap-1 px-3">
        <div className="flex min-w-0 flex-1 items-center gap-1">
          {scope && status?.isRepo ? (
            <>
              <BranchPicker scope={scope} status={status} />
              {status.upstream ? (
                status.behind > 0 || status.ahead > 0 ? (
                  <span className="flex shrink-0 items-center gap-1 font-mono text-[11px] text-muted-foreground">
                    {status.behind > 0 ? (
                      <span className="flex items-center text-cyan">
                        <ArrowDown className="size-3" />
                        {status.behind}
                      </span>
                    ) : null}
                    {status.ahead > 0 ? (
                      <span className="flex items-center text-foreground">
                        <ArrowUp className="size-3" />
                        {status.ahead}
                      </span>
                    ) : null}
                  </span>
                ) : null
              ) : (
                <span className="truncate text-[11px] text-faint">not published</span>
              )}
            </>
          ) : scope && statusLoading ? (
            <span className="truncate pl-0.5 text-[12px] text-faint">Loading…</span>
          ) : (
            <span className="truncate pl-0.5 text-[12.5px] font-medium text-foreground">{project?.name ?? "Workspace"}</span>
          )}
          {running ? (
            <Tip content="Agent is working" side="bottom">
              <LoaderCircle className="ml-1 size-3.5 shrink-0 animate-spin text-muted-foreground" aria-label="Working" />
            </Tip>
          ) : null}
        </div>

        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          {scope && status?.isRepo ? (
            <>
              <SyncMenu scope={scope} status={status} />
              <span className="mx-0.5 h-3.5 w-px shrink-0 bg-border" aria-hidden="true" />
            </>
          ) : null}
          {meta ? <SessionMenu meta={meta} /> : null}
          <Tip content="Collapse control center" side="bottom">
            <button
              type="button"
              className="grid size-6 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="Collapse control center"
              onClick={() => setLayout({ controlCollapsed: true })}
            >
              <ChevronUp className="size-3.5" />
            </button>
          </Tip>
        </div>
      </div>

      {/* Section 2: Live Subagents */}
      {subagents.length > 0 ? (
        <div className={cn("flex flex-col border-t", expanded === "subagents" && "min-h-0 flex-1")}>
          <div className="shrink-0 p-1.5">
            <button
              type="button"
              className={cn(
                "flex h-7 w-full items-center gap-2 rounded-lg px-2 text-left text-[12.5px] hover:bg-accent",
                expanded === "subagents" && "bg-accent",
              )}
              aria-expanded={expanded === "subagents"}
              onClick={() => toggle("subagents")}
            >
              {runningSubagents.length > 0 ? (
                <span className="ml-0.5 size-[7px] shrink-0 rounded-full bg-green animate-pulse" aria-label="Running" />
              ) : (
                <Check className="size-3.5 shrink-0 text-green" aria-label="Completed" />
              )}
              <span className="font-medium text-foreground">Subagents</span>
              <span
                className={cn(
                  "ml-auto rounded-full border px-1.5 py-px font-mono text-[10px] font-medium",
                  runningSubagents.length > 0
                    ? "border-green/30 bg-green/10 text-green"
                    : "border-border bg-secondary text-muted-foreground",
                )}
              >
                {runningSubagents.length > 0 ? `${runningSubagents.length} running` : `${subagents.length} done`}
              </span>
              <ChevronDown className={cn("size-3 shrink-0 text-faint transition-transform", expanded === "subagents" && "rotate-180")} />
            </button>

            {/* Compact live preview rows in resting state */}
            {expanded === null && runningSubagents.length > 0 ? (
              <div className="mt-0.5 flex flex-col gap-0.5">
                {runningSubagents.slice(0, 3).map((item) => {
                  const elapsed = item.startedAt !== undefined ? formatDuration(Math.max(0, now - item.startedAt)) : null
                  return (
                    <button
                      key={item.id}
                      type="button"
                      className="flex h-[26px] w-full items-center gap-2 rounded-md pr-2 pl-6 text-left text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground"
                      onClick={() => (onOpenSubagent ? onOpenSubagent(item) : item.itemId ? onJumpToItem?.(item.itemId) : toggle("subagents"))}
                    >
                      <span className="min-w-0 flex-1 truncate">{item.title}</span>
                      {elapsed ? <span className="shrink-0 font-mono text-[10.5px] text-faint tabular-nums">{elapsed}</span> : null}
                      <ChevronRight className="size-3 shrink-0 text-faint" />
                    </button>
                  )
                })}
              </div>
            ) : null}
          </div>

          {expanded === "subagents" ? (
            <div className="min-h-0 flex-1 overflow-y-auto border-t">
              {runningSubagents.length > 0 ? (
                <SubagentsPanel
                  subagents={runningSubagents}
                  open
                  stepsById={stepsById}
                  onStop={onStopTask}
                  onOpenSubagent={onOpenSubagent}
                  onJumpToItem={onJumpToItem}
                />
              ) : null}
              {finishedSubagents.length > 0 ? (
                <div className={cn("flex flex-col gap-1 p-2", runningSubagents.length > 0 && "border-t")}>
                  {finishedSubagents.map((item) => {
                    const dur =
                      item.startedAt !== undefined && item.endedAt !== undefined
                        ? formatDuration(Math.max(0, item.endedAt - item.startedAt))
                        : null
                    const label = item.status === "completed" ? "done" : item.status
                    return (
                      <button
                        key={item.id}
                        type="button"
                        className="flex w-full items-center gap-2 rounded-lg border border-border/60 bg-secondary/40 px-2.5 py-1.5 text-left hover:bg-accent"
                        onClick={() => (onOpenSubagent ? onOpenSubagent(item) : item.itemId ? onJumpToItem?.(item.itemId) : undefined)}
                      >
                        {item.status === "failed" || item.status === "stopped" ? (
                          <X className="size-3.5 shrink-0 text-red" />
                        ) : (
                          <Check className="size-3.5 shrink-0 text-green" />
                        )}
                        <span className="min-w-0 flex-1 truncate text-[12px] font-medium text-foreground/90">{item.title}</span>
                        <span className="shrink-0 font-mono text-[10.5px] text-faint tabular-nums">
                          {dur ? `${label} · ${dur}` : label}
                        </span>
                      </button>
                    )
                  })}
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}

      {/* Section 3: Source Control */}
      {scope ? (
        <div className={cn("flex flex-col border-t", expanded === "scm" && "min-h-0 flex-1")}>
          <div className="shrink-0 p-1.5">
            <button
              type="button"
              className={cn(
                "flex h-7 w-full items-center gap-2 rounded-lg px-2 text-left text-[12.5px] hover:bg-accent",
                expanded === "scm" ? "bg-accent text-foreground" : "text-foreground/90",
              )}
              aria-expanded={expanded === "scm"}
              onClick={() => toggle("scm")}
            >
              <FileDiff className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate">Source control</span>
              {totals.count > 0 ? (
                <span className="rounded-full border border-border bg-secondary px-1.5 font-mono text-[10px] font-semibold leading-4 text-foreground/85">
                  {totals.count > 99 ? "99" : totals.count}
                </span>
              ) : null}
              <span className="ml-auto flex shrink-0 items-center gap-1.5">
                {totals.added > 0 || totals.removed > 0 ? (
                  <span className="font-mono text-[10.5px]">
                    <span className="text-green">+{totals.added}</span> <span className="text-red">−{totals.removed}</span>
                  </span>
                ) : null}
                <ChevronDown className={cn("size-3 shrink-0 text-faint transition-transform", expanded === "scm" && "rotate-180")} />
              </span>
            </button>
          </div>
          {expanded === "scm" ? (
            <div className="min-h-0 flex-1 overflow-y-auto border-t">
              <SourceControl scope={scope} hideBranchBar />
            </div>
          ) : null}
        </div>
      ) : null}

      {/* Expanded Workspace Panel (Files / History / Pull Request) */}
      {scope && workspaceExpanded ? (
        expanded === "pr" ? (
          <div className="min-h-0 flex-1 overflow-y-auto border-t">
            <PullRequest scope={scope} />
          </div>
        ) : (
          <div className="flex h-[340px] min-h-0 shrink flex-col overflow-hidden border-t">
            {expanded === "files" ? <Files scope={scope} /> : <History scope={scope} />}
          </div>
        )
      ) : null}

      {/* Bottom 3-Segment Workspace Strip */}
      {scope ? (
        <div className="flex shrink-0 items-center gap-1 border-t p-1.5">
          {WORKSPACE_TOOLS.map((tool) => {
            const active = expanded === tool.id
            return (
              <button
                key={tool.id}
                type="button"
                aria-pressed={active}
                className={cn(
                  "flex h-[26px] flex-1 items-center justify-center gap-1.5 rounded-lg border text-[11.5px] font-medium transition-colors",
                  active
                    ? "border-border bg-accent text-foreground"
                    : "border-transparent bg-secondary text-muted-foreground hover:border-border hover:bg-accent hover:text-foreground",
                )}
                onClick={() => toggle(tool.id)}
              >
                <tool.icon className="size-3 shrink-0" />
                <span>{tool.label}</span>
              </button>
            )
          })}
        </div>
      ) : null}
    </aside>
  )
}

function SyncMenu({ scope, status }: { scope: Scope; status: Status }) {
  const run = useGit((s) => s.run)
  const busy = useGit((s) => s.busy)
  return (
    <DropdownMenu>
      <Tip content="Sync" side="bottom">
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className="grid size-6 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="Sync"
          >
            <RefreshCw className={cn("size-3.5", busy && "animate-spin")} />
          </button>
        </DropdownMenuTrigger>
      </Tip>
      <DropdownMenuContent align="end" className="w-44">
        <DropdownMenuItem onSelect={() => run(scope, "Syncing…", () => git.sync(scope, "sync"))}>
          <RefreshCw /> Sync: pull and push
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => run(scope, "Pulling…", () => git.sync(scope, "pull"))}>
          <ArrowDown /> Pull
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => run(scope, "Pushing…", () => git.sync(scope, "push"))}>
          <ArrowUp /> {status.upstream ? "Push" : "Publish branch"}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => run(scope, "Fetching…", () => git.sync(scope, "fetch"))}>
          <CloudDownload /> Fetch
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function SessionMenu({ meta }: { meta: ChatMeta }) {
  return (
    <DropdownMenu>
      <Tip content="Chat menu" side="bottom">
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className="grid size-6 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="Chat menu"
          >
            <MoreHorizontal className="size-4" />
          </button>
        </DropdownMenuTrigger>
      </Tip>
      <DropdownMenuContent align="end" className="w-auto min-w-56">
        <DropdownMenuItem
          onSelect={() => {
            void promptText({ title: "Rename chat", value: meta.title, confirm: "Rename" }).then((t) => {
              if (t) void useChats.getState().rename(meta.id, t)
            })
          }}
        >
          Rename
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => void copyChatMarkdown(meta.id)}>
          <FileText /> Copy as Markdown
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => copyText(meta.agentSessionId ?? "")} disabled={!meta.agentSessionId}>
          Copy session ID
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onSelect={() => void useChats.getState().confirmRemove(meta.id)}>
          <Trash2 /> Delete chat
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
