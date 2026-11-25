import { useEffect, useMemo, useRef, useState, type MouseEvent, type PointerEvent, type ReactNode } from "react"
import { ChevronDown, ChevronRight, FlaskConical, Gauge, ListChecks, Loader2, MessageCircleQuestion, Moon, Pencil, Pin, PinOff, SquarePen, SquareTerminal, Trash2 } from "lucide-react"
import { LocationIcon } from "@/components/LocationIcon"
import { AgentGlyph, Mark } from "@/components/icons"
import type { ChatMeta } from "@/lib/chat"
import { AGENT_LABEL, type Project } from "@/lib/types"
import { cn } from "@/lib/utils"
import { DesktopList } from "./Desktops"
import { NewChatSheet } from "./NewChat"
import { PhoneSettings } from "./Settings"
import { UsageSheet } from "./Usage"
import { go } from "./route"
import { tap, type Waiting } from "./rpc"
import { PullSheet } from "./sheet"
import { answered, useMobile } from "./store"
import { Capsule, LinkBanner, RoundButton, UNDER_CAPSULE, ago } from "./ui"

/** The folder a chat works in, when it's a worktree rather than the project itself. */
function worktreeOf(meta: ChatMeta, project: Project | undefined): string | null {
  if (!project || !meta.cwd || meta.cwd.replace(/[\\/]+$/, "") === project.path.replace(/[\\/]+$/, "")) return null
  return meta.cwd.split(/[\\/]/).filter(Boolean).pop() ?? null
}

const WAITING_LABEL: Record<Waiting, string> = { approval: "Approval", question: "Question", plan: "Plan" }
const open = (id: string) => go(`/chat/${encodeURIComponent(id)}`)

/** Press and hold: `onLong` after a moment, unless the finger moves (a scroll) or lifts first.
 *  The tap that ends a long press doesn't also count as a click. */
function useLongPress(onLong: () => void, ms = 450) {
  const timer = useRef(0)
  const from = useRef<{ x: number; y: number } | null>(null)
  const fired = useRef(false)
  const cancel = () => {
    clearTimeout(timer.current)
    from.current = null
  }
  useEffect(() => cancel, [])
  return {
    onPointerDown: (e: PointerEvent) => {
      fired.current = false
      from.current = { x: e.clientX, y: e.clientY }
      timer.current = window.setTimeout(() => {
        fired.current = true
        from.current = null
        tap()
        onLong()
      }, ms)
    },
    onPointerMove: (e: PointerEvent) => {
      if (from.current && Math.hypot(e.clientX - from.current.x, e.clientY - from.current.y) > 10) cancel()
    },
    onPointerUp: cancel,
    onPointerCancel: cancel,
    onPointerLeave: cancel,
    // Android's own long-press menu (and text selection) would get in the way.
    onContextMenu: (e: MouseEvent) => e.preventDefault(),
    onClickCapture: (e: MouseEvent) => {
      if (!fired.current) return
      fired.current = false
      e.preventDefault()
      e.stopPropagation()
    },
  }
}

/** A chat card's top line: where (or what it waits for), the agent, and when. */
function CardMeta({ meta, project, waiting }: { meta: ChatMeta; project: Project | undefined; waiting?: Waiting }) {
  const busy = useMobile((s) => s.busy.includes(meta.id))
  return (
    <span className="flex min-w-0 items-center gap-1.5 text-[12px] text-muted-foreground">
      {waiting ? (
        <>
          <span className="size-2 shrink-0 rounded-full bg-primary" />
          <span className="shrink-0 font-medium text-foreground">{WAITING_LABEL[waiting]}</span>
          <span className="text-faint">·</span>
        </>
      ) : project ? (
        <LocationIcon location={project.location} className="size-3.5" />
      ) : null}
      <span className="min-w-0 truncate">{project?.name}</span>
      <span className="text-faint">·</span>
      <AgentGlyph agent={meta.agent} className="size-3.5 shrink-0" />
      {waiting ? null : <span className="shrink-0">{AGENT_LABEL[meta.agent]}</span>}
      {meta.pinned ? <Pin className="size-3 shrink-0 text-faint" aria-label="Pinned" /> : null}
      {meta.sleeping ? <Moon className="size-3 shrink-0 text-faint" aria-label="Asleep" /> : null}
      <span className="ml-auto shrink-0 pl-1 font-mono text-[11px] text-faint">{busy && !waiting ? <Loader2 className="size-3.5 animate-spin" aria-label="Working" /> : ago(meta.updatedAt)}</span>
    </span>
  )
}

const PILL = "flex h-11 items-center justify-center gap-1.5 rounded-full px-4 text-[14px] disabled:opacity-50"

/** A chat that waits for you, answered right here when it can be: Allow or Deny an approval,
 *  or tap an option of a one-pick question. Plans, and questions with more to them, open. */
function NeedsYouCard({ meta, project, waiting, onMenu }: { meta: ChatMeta; project: Project | undefined; waiting: Waiting; onMenu: (m: ChatMeta) => void }) {
  const request = useMobile((s) => s.requests[meta.id])
  const connected = useMobile((s) => s.link === "connected")
  const press = useLongPress(() => onMenu(meta))
  const [sent, setSent] = useState<string | null>(null)
  const st = useMobile.getState()
  const approval = request?.approval
  const question = request?.question
  const current = approval ?? question
  const done = !!current && (answered.has(current.requestId) || sent === current.requestId)
  const q = question?.questions[0]
  const simple = !!q && question!.questions.length === 1 && !q.multi && !q.secret && q.options.length > 0

  let body: ReactNode
  if (done) {
    body = <div className="mt-2.5 flex h-11 items-center gap-2 px-1 text-[13px] text-muted-foreground"><Loader2 className="size-4 animate-spin" /> Sent. Waiting for {AGENT_LABEL[meta.agent]}…</div>
  } else if (waiting === "plan" || approval?.kind === "plan") {
    body = (
      <button className={cn(PILL, "mt-3 w-full bg-primary font-semibold text-primary-foreground")} onClick={() => open(meta.id)}>
        <ListChecks className="size-4" /> Review the plan
      </button>
    )
  } else if (approval) {
    body = (
      <>
        <div className="mt-2.5 flex items-center gap-3 rounded-[18px] bg-background px-3 py-2.5">
          <SquareTerminal className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0">
            <div className="truncate text-[13.5px]">{approval.title}</div>
            {approval.detail ? <div className={cn("truncate text-[12px] text-muted-foreground", approval.kind === "command" && "font-mono")}>{approval.detail}</div> : null}
            {approval.diff ? <div className="font-mono text-[12px]"><span className="text-green">+{approval.diff.added}</span> <span className="text-red">−{approval.diff.removed}</span></div> : null}
          </div>
        </div>
        <div className="mt-3 flex gap-2">
          <button className={cn(PILL, "flex-[1.4] bg-primary font-semibold text-primary-foreground")} disabled={!connected} onClick={() => (setSent(approval.requestId), void st.answer(meta.id, approval.requestId, "allow"))}>
            Allow
          </button>
          <button className={cn(PILL, "flex-1 border border-input text-red")} disabled={!connected} onClick={() => (setSent(approval.requestId), void st.answer(meta.id, approval.requestId, "deny"))}>
            Deny
          </button>
          <RoundButton label="Open the chat" className="size-11 border border-input" onClick={() => open(meta.id)}>
            <ChevronRight className="size-5" />
          </RoundButton>
        </div>
      </>
    )
  } else if (question && q) {
    body = (
      <>
        <div className="mt-2 flex items-start gap-2 text-[15px] leading-snug font-semibold">
          <MessageCircleQuestion className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0">{q.question}</span>
        </div>
        <div className="mt-2.5 flex flex-wrap gap-1.5">
          {simple
            ? q.options.map((o) => (
                <button
                  key={o.label}
                  className="h-10 max-w-full truncate rounded-full border border-input bg-background px-3.5 text-[13.5px] active:bg-accent disabled:opacity-50"
                  disabled={!connected}
                  onClick={() => (setSent(question.requestId), void st.answerQuestion(meta.id, question.requestId, { [q.id]: [o.label] }))}
                >
                  {o.label}
                </button>
              ))
            : null}
          <button className={cn("flex h-10 items-center gap-0.5 rounded-full pr-2 pl-3 text-[13.5px] active:bg-accent", simple ? "text-muted-foreground" : "border border-input")} onClick={() => open(meta.id)}>
            {simple ? (q.other ? "Other" : "Open") : "Answer"} <ChevronRight className="size-4" />
          </button>
        </div>
      </>
    )
  } else {
    body = (
      <button className={cn(PILL, "mt-3 w-full border border-input")} onClick={() => open(meta.id)}>
        {request ? "Open the chat" : <><Loader2 className="size-4 animate-spin text-muted-foreground" /> Open the chat</>}
      </button>
    )
  }

  return (
    <div className="rounded-[26px] border border-primary/30 bg-card p-3.5 select-none [-webkit-touch-callout:none]" {...press}>
      <button className="block w-full text-left" onClick={() => open(meta.id)}>
        <CardMeta meta={meta} project={project} waiting={waiting} />
        <span className={cn("mt-1 line-clamp-2 block text-[16px] leading-snug font-semibold", !meta.title && "text-muted-foreground")}>{meta.title || "New chat"}</span>
      </button>
      {body}
    </div>
  )
}

/** Any other chat, as a round card: hold it for its menu. */
function ChatCard({ meta, project, onMenu }: { meta: ChatMeta; project: Project | undefined; onMenu: (m: ChatMeta) => void }) {
  const busy = useMobile((s) => s.busy.includes(meta.id))
  const worktree = worktreeOf(meta, project)
  const press = useLongPress(() => onMenu(meta))
  return (
    <button className="flex w-full flex-col gap-1 rounded-[26px] border border-transparent bg-card px-4 py-3.5 text-left select-none [-webkit-touch-callout:none] active:bg-accent dark:border-white/[0.04]" onClick={() => open(meta.id)} {...press}>
      <CardMeta meta={meta} project={project} />
      <span className={cn("line-clamp-2 text-[16px] leading-snug font-medium", !meta.title && "text-muted-foreground")}>{meta.title || "New chat"}</span>
      {busy || worktree || meta.queued?.length ? (
        <span className="flex min-w-0 items-center gap-2 text-[12.5px]">
          {busy ? <span className="shimmer shrink-0">Working…</span> : null}
          {worktree ? <span className="min-w-0 truncate rounded-full bg-muted px-2 py-0.5 font-mono text-[11px] text-muted-foreground">{worktree}</span> : null}
          {meta.queued?.length ? <span className="shrink-0 text-muted-foreground">{meta.queued.length} queued</span> : null}
        </span>
      ) : null}
    </button>
  )
}

/** A chat's menu (hold its card, or ⋯ in it): pin, sleep, rename, delete. */
export function ChatMenu({ meta, onClose, onDeleted }: { meta: ChatMeta | null; onClose: () => void; onDeleted?: () => void }) {
  const busy = useMobile((s) => (meta ? s.busy.includes(meta.id) : false))
  const waiting = useMobile((s) => (meta ? !!s.waiting[meta.id] : false))
  const connected = useMobile((s) => s.link === "connected")
  const project = useMobile((s) => s.projects.find((p) => p.id === meta?.projectId))
  const [renaming, setRenaming] = useState(false)
  const [title, setTitle] = useState("")
  // Deleting asks with a second tap (Android's WebView may not show confirm dialogs).
  const [armed, setArmed] = useState(false)
  useEffect(() => {
    setRenaming(false)
    setArmed(false)
    setTitle(meta?.title ?? "")
  }, [meta?.id])
  useEffect(() => {
    if (!armed) return
    const t = setTimeout(() => setArmed(false), 3000)
    return () => clearTimeout(t)
  }, [armed])

  const st = useMobile.getState()
  const sleepBlocked = !meta ? null : meta.sleeping ? "Already asleep" : busy ? "Working" : waiting ? "Waiting for you" : !meta.agentSessionId ? "Nothing to resume yet" : null
  const tile = "flex h-[76px] flex-col items-center justify-center gap-1.5 rounded-[20px] bg-muted px-1 text-center text-[12.5px] leading-tight active:bg-accent disabled:opacity-40"
  const save = () => {
    if (meta && title.trim()) void st.rename(meta.id, title.trim())
    onClose()
  }

  return (
    <PullSheet
      open={!!meta}
      onClose={onClose}
      title={meta?.title || "New chat"}
      header={
        meta ? (
          <div className="flex items-center gap-3 px-4 pt-1 pb-3">
            <span className="grid size-10 shrink-0 place-items-center rounded-full bg-muted">
              <AgentGlyph agent={meta.agent} className="size-[18px]" />
            </span>
            <div className="min-w-0">
              <div className="line-clamp-2 text-[16px] leading-snug font-semibold">{meta.title || "New chat"}</div>
              <div className="truncate text-[12.5px] text-muted-foreground">{[project?.name, AGENT_LABEL[meta.agent], ago(meta.updatedAt)].filter(Boolean).join(" · ")}</div>
            </div>
          </div>
        ) : null
      }
    >
      {!meta ? null : renaming ? (
        <form
          className="flex gap-2 px-3 pb-2"
          onSubmit={(e) => {
            e.preventDefault()
            save()
          }}
        >
          <input
            autoFocus
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            aria-label="Chat name"
            className="h-12 min-w-0 flex-1 rounded-full border border-input bg-background px-4 text-[15px] outline-none focus:border-ring"
          />
          <button type="submit" disabled={!title.trim()} className="h-12 shrink-0 rounded-full bg-primary px-5 text-[14px] font-semibold text-primary-foreground disabled:opacity-50">
            Save
          </button>
        </form>
      ) : (
        <div className="px-3 pb-1">
          <div className="grid grid-cols-3 gap-1.5">
            <button className={tile} disabled={!connected} onClick={() => (void st.pin(meta.id, !meta.pinned), onClose())}>
              {meta.pinned ? <PinOff className="size-5 text-muted-foreground" /> : <Pin className="size-5 text-muted-foreground" />}
              {meta.pinned ? "Unpin" : "Pin"}
            </button>
            <button className={tile} disabled={!connected || !!sleepBlocked} onClick={() => (void st.sleep(meta.id), onClose())}>
              <Moon className="size-5 text-muted-foreground" />
              <span>Sleep{sleepBlocked ? <span className="block text-[11px] text-muted-foreground">{sleepBlocked}</span> : null}</span>
            </button>
            <button className={tile} disabled={!connected} onClick={() => setRenaming(true)}>
              <Pencil className="size-5 text-muted-foreground" />
              Rename
            </button>
          </div>
          <button
            className={cn("mt-1.5 flex h-12 w-full items-center justify-center gap-2 rounded-[20px] bg-red/10 text-[14.5px] text-red disabled:opacity-40", armed && "bg-red/20 font-medium")}
            disabled={!connected}
            onClick={() => {
              if (!armed) return setArmed(true)
              void st.remove(meta.id)
              onClose()
              onDeleted?.()
            }}
          >
            <Trash2 className="size-[18px]" /> {armed ? "Tap again to delete" : "Delete chat"}
          </button>
        </div>
      )}
    </PullSheet>
  )
}

/** The project filter, kept while going in and out of chats. */
let lastFilter = "all"

export function ChatsScreen({ usage }: { usage: boolean }) {
  const name = useMobile((s) => s.desktopName)
  const link = useMobile((s) => s.link)
  const projects = useMobile((s) => s.projects)
  const metas = useMobile((s) => s.metas)
  const waiting = useMobile((s) => s.waiting)
  const loadedOnce = useMobile((s) => s.listed || s.projects.length > 0 || s.metas.length > 0)
  const demo = useMobile((s) => s.desktops.find((d) => d.id === s.desktopId)?.demo ?? false)
  const [menu, setMenu] = useState<ChatMeta | null>(null)
  const [pull, setPull] = useState(false)
  const [newChat, setNewChat] = useState(false)
  const [filter, setFilterState] = useState(lastFilter)
  const [limit, setLimit] = useState(40)
  const setFilter = (f: string) => setFilterState((lastFilter = f))
  const project = projects.find((p) => p.id === filter)
  const projectOf = useMemo(() => new Map(projects.map((p) => [p.id, p])), [projects])

  // What each waiting chat asks: read once per change in the list, again after an answer.
  useEffect(() => {
    if (link !== "connected") return
    const { requests, peek } = useMobile.getState()
    for (const id of Object.keys(waiting)) {
      const r = requests[id]
      const current = r?.approval ?? r?.question
      if (!r || (current && answered.has(current.requestId))) void peek(id)
    }
  }, [waiting, link])

  const shown = filter === "all" || !project ? metas : metas.filter((m) => m.projectId === filter)
  const needsYou = shown.filter((m) => waiting[m.id])
  const rest = useMemo(() => shown.filter((m) => !waiting[m.id]).sort((a, b) => Number(b.pinned) - Number(a.pinned)), [shown, waiting])
  const busyCount = useMobile((s) => s.busy.filter((id) => !s.waiting[id]).length)
  const summary = [needsYou.length ? `${needsYou.length} need${needsYou.length === 1 ? "s" : ""} you` : null, busyCount ? `${busyCount} working` : null].filter(Boolean).join(" · ")

  // A recent project first, unless one is picked.
  const initialProject = project?.id ?? metas[0]?.projectId ?? projects[0]?.id ?? null
  const chip = "flex h-9 shrink-0 items-center gap-1.5 rounded-full px-3.5 text-[13px] active:bg-accent"

  return (
    <div className="relative h-full bg-background">
      <div className={cn("h-full overflow-y-auto pb-40", UNDER_CAPSULE)}>
        <LinkBanner className="mx-3 mb-2" />
        {demo ? (
          <div className="mx-2.5 mb-2 flex items-center gap-3 rounded-[20px] border border-dashed px-3.5 py-1.5 text-[12.5px] text-muted-foreground">
            <FlaskConical className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 py-1 leading-snug">Demo mode. Nothing here runs on a real computer.</span>
            <button className="h-9 shrink-0 rounded-full bg-muted px-3.5 font-medium text-foreground active:bg-accent" onClick={() => go("/pair")}>
              Pair one
            </button>
          </div>
        ) : null}
        {projects.length > 1 ? (
          <div className="flex gap-2 overflow-x-auto px-3 pb-1" role="group" aria-label="Show chats from">
            <button aria-pressed={filter === "all" || !project} className={cn(chip, filter === "all" || !project ? "bg-primary font-semibold text-primary-foreground" : "bg-muted")} onClick={() => setFilter("all")}>
              All
            </button>
            {projects.map((p) => (
              <button key={p.id} aria-pressed={filter === p.id} className={cn(chip, filter === p.id ? "bg-primary font-semibold text-primary-foreground" : "bg-muted")} onClick={() => setFilter(p.id)}>
                {p.location.kind === "local" ? null : <LocationIcon location={p.location} className={filter === p.id ? "text-primary-foreground" : undefined} />}
                {p.name}
              </button>
            ))}
          </div>
        ) : null}
        <div className="px-2.5">
          {!loadedOnce ? (
            <div className="flex h-48 flex-col items-center justify-center gap-2 px-8 text-center text-[13px] text-muted-foreground">
              <Loader2 className="size-5 animate-spin" />
              {link === "connected" ? "Loading…" : "Waiting for the computer. Open Moonfish there and connect both devices to the same Wi-Fi network or VPN."}
            </div>
          ) : projects.length === 0 ? (
            <div className="px-6 py-16 text-center text-[13px] text-muted-foreground">No projects on this desktop yet. Open a folder in Moonfish there first.</div>
          ) : (
            <>
              {needsYou.length ? (
                <>
                  <Label count={needsYou.length}>Needs you</Label>
                  <div className="flex flex-col gap-2">
                    {needsYou.map((m) => <NeedsYouCard key={m.id} meta={m} project={projectOf.get(m.projectId)} waiting={waiting[m.id]} onMenu={setMenu} />)}
                  </div>
                </>
              ) : null}
              <Label aside={summary && !needsYou.length ? summary : undefined}>{needsYou.length ? "Recent" : "Chats"}</Label>
              {rest.length ? (
                <div className="flex flex-col gap-2">
                  {rest.slice(0, limit).map((m) => <ChatCard key={m.id} meta={m} project={projectOf.get(m.projectId)} onMenu={setMenu} />)}
                  {rest.length > limit ? (
                    <button className="mx-auto mt-1 h-10 rounded-full bg-muted px-4 text-[13px] active:bg-accent" onClick={() => setLimit(limit + 40)}>
                      Show more
                    </button>
                  ) : null}
                </div>
              ) : (
                <div className="rounded-[26px] border border-dashed px-4 py-6 text-center text-[13px] text-muted-foreground">
                  {needsYou.length ? "Nothing else here." : `No chats${project ? ` in ${project.name}` : ""} yet. Start one with New chat.`}
                </div>
              )}
            </>
          )}
        </div>
      </div>

      <Capsule>
        <button className="flex h-10 min-w-0 items-center gap-2 rounded-full pr-3 pl-0.5 active:bg-accent" aria-label={`${name || "Desktop"}, desktops and settings`} onClick={() => setPull(true)}>
          <span className="grid size-9 shrink-0 place-items-center rounded-full bg-background">
            <Mark size={18} />
          </span>
          <span className="truncate text-[14.5px] font-semibold">{name || "Moonfish"}</span>
          <span
            className={cn("size-2 shrink-0 rounded-full", link === "connected" ? "bg-green" : link === "connecting" ? "animate-pulse bg-muted-foreground" : "bg-red")}
            aria-label={link === "connected" ? "Connected" : link === "connecting" ? "Connecting" : "Offline"}
          />
          <ChevronDown className="size-4 shrink-0 text-muted-foreground" />
        </button>
        <span className="flex-1" />
        <RoundButton label="Usage" onClick={() => go("/usage")}>
          <Gauge className="size-[19px]" />
        </RoundButton>
      </Capsule>

      <div className="pointer-events-none absolute inset-x-0 bottom-0 z-10 h-44 bg-gradient-to-t from-background from-35% via-background/85 to-transparent" />
      <div className="pointer-events-none absolute inset-x-4 bottom-[calc(var(--safe-bottom)+16px)] z-20 flex flex-col gap-2">
        <div className="flex items-center gap-2 [&>*]:pointer-events-auto">
          <span className="flex-1" />
          <button
            className="flex h-14 shrink-0 items-center gap-2 rounded-full bg-primary pr-6 pl-5 text-[15px] font-semibold text-primary-foreground shadow-[0_12px_30px_-10px_rgb(245_184_0/0.5)] active:bg-primary/90 disabled:opacity-50"
            disabled={link !== "connected" || !projects.length}
            onClick={() => setNewChat(true)}
          >
            <SquarePen className="size-[18px]" /> New chat
          </button>
        </div>
      </div>

      <PullSheet open={pull} onClose={() => setPull(false)} side="top" title="Desktops and this phone">
        <div className="min-h-0 overflow-y-auto px-2.5 pt-3">
          <div className="px-2.5 pb-2 text-[12.5px] font-medium text-muted-foreground">Desktops</div>
          <DesktopList onPicked={() => setPull(false)} />
          <div className="px-2.5 pt-4 pb-2 text-[12.5px] font-medium text-muted-foreground">This phone</div>
          <PhoneSettings />
        </div>
      </PullSheet>
      {usage ? <UsageSheet /> : null}
      <NewChatSheet open={newChat} initial={initialProject} onClose={() => setNewChat(false)} />
      {/* The newest copy of the chat, so pin and sleep show what they are now. */}
      <ChatMenu meta={menu ? (metas.find((m) => m.id === menu.id) ?? null) : null} onClose={() => setMenu(null)} />
    </div>
  )
}

function Label({ children, count, aside }: { children: string; count?: number; aside?: string }) {
  return (
    <div className="flex items-center gap-2 px-2 pt-3 pb-2">
      <h2 className="text-[13px] font-semibold">{children}</h2>
      {count ? <span className="grid h-5 min-w-5 place-items-center rounded-full bg-primary px-1.5 text-[11px] font-bold text-primary-foreground">{count}</span> : null}
      {aside ? <span className="ml-auto text-[12px] text-muted-foreground">{aside}</span> : null}
    </div>
  )
}
