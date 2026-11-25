import { useGoalControls } from "@/components/chat/GoalPanel"
import { supportsGoals } from "@/lib/goals"
import { useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode } from "react"
import { ChevronUp, MoreHorizontal, X } from "lucide-react"
import { toast } from "sonner"
import { ApprovalCard } from "@/components/chat/Approval"
import { ImageSourceContext, type ImageSource } from "@/components/chat/ChatImages"
import { PlanDock } from "@/components/chat/PlanDock"
import { QuestionCard } from "@/components/chat/QuestionCard"
import { SubagentsPanel, useSubagentSteps } from "@/components/chat/SubagentsMenu"
import { isDockTask, TasksDock } from "@/components/chat/TasksDock"
import { Transcript, useDock, type JumpTarget } from "@/components/chat/Transcript"
import { CwdContext } from "@/components/chat/cwd"
import { emptyTranscript } from "@/lib/chat"
import { extractSubagents, subagentSummary } from "@/lib/subagents"
import { AGENT_LABEL } from "@/lib/types"
import { ChatMenu } from "./Chats"
import { MobileComposer } from "./Composer"
import { useCatalog } from "./models"
import { api, native } from "./rpc"
import { go } from "./route"
import { Grabber, PullSheet, SheetHead } from "./sheet"
import { useMobile } from "./store"
import { LinkBanner, RoundButton, TitleCapsule } from "./ui"

const NONE: never[] = []

/** What the chat asks for, risen over the composer as a sheet. Pull it down (or tap its handle)
 *  to read the chat behind it; it comes back with a tap. */
function WaitingSheet({ title, lowered, onLowered, children, composer }: { title: string; lowered: boolean; onLowered: (v: boolean) => void; children: ReactNode; composer: ReactNode }) {
  const from = useRef<number | null>(null)
  const handle = {
    onPointerDown: (e: PointerEvent) => {
      from.current = e.clientY
      e.currentTarget.setPointerCapture(e.pointerId)
    },
    onPointerUp: (e: PointerEvent) => {
      if (from.current === null) return
      const dy = e.clientY - from.current
      from.current = null
      onLowered(dy > 24 ? true : dy < -24 ? false : !lowered)
    },
    onPointerCancel: () => void (from.current = null),
  }
  return (
    <div className="rounded-t-[32px] border-t bg-card shadow-[0_-18px_44px_-14px_rgb(0_0_0/0.55)]">
      <button className="block w-full touch-none pt-2.5 pb-1.5" aria-label={lowered ? "Show what it asks" : "Lower it to read the chat"} {...handle}>
        <Grabber />
      </button>
      {lowered ? (
        <button className="flex w-full items-center gap-2 px-4 pb-1 text-left" onClick={() => onLowered(false)}>
          <span className="size-2 shrink-0 rounded-full bg-primary" />
          <span className="min-w-0 flex-1 truncate text-[14px] font-medium">{title}</span>
          <span className="flex shrink-0 items-center gap-0.5 text-[12.5px] text-muted-foreground">
            Answer <ChevronUp className="size-4" />
          </span>
        </button>
      ) : (
        children
      )}
      {composer}
    </div>
  )
}

export function ChatScreen({ chatId }: { chatId: string }) {
  const desktopId = useMobile((s) => s.desktopId)
  const meta = useMobile((s) => s.metas.find((m) => m.id === chatId))
  const listed = useMobile((s) => s.listed)
  const transcript = useMobile((s) => s.transcripts[chatId])
  const link = useMobile((s) => s.link)
  const queued = meta?.queued ?? NONE
  const agents = useMobile((s) => s.agents)
  const project = useMobile((s) => s.projects.find((p) => p.id === meta?.projectId))
  const st = useMobile.getState()
  const [menu, setMenu] = useState(false)
  const [subagentsOpen, setSubagentsOpen] = useState(false)
  const [lowered, setLowered] = useState(false)
  const [jumpTarget, setJumpTarget] = useState<JumpTarget | null>(null)
  const jumpSeq = useRef(0)
  const jumpToItem = (id: string) => setJumpTarget({ id, seq: ++jumpSeq.current })

  useEffect(() => {
    if (listed && link === "connected" && (!meta || !project)) go("/", true)
  }, [listed, link, meta, project])

  useEffect(() => {
    void st.open(chatId)
    // On screen now: its notification (Settings → Alerts) goes.
    void native.alertsSeen(chatId).catch(() => {})
    return () => useMobile.getState().leave()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chatId])

  const agent = agents.find((a) => a.kind === meta?.agent)
  const t = transcript ?? emptyTranscript()
  const subagents = useMemo(() => extractSubagents(t), [t])
  const runningSubagents = useMemo(() => subagents.filter((s) => s.status === "running"), [subagents])
  const dockTasks = useMemo(() => t.tasks.filter(isDockTask), [t.tasks])
  const subagentSteps = useSubagentSteps(runningSubagents)
  const { models, commands, load, failed: modelsFailed, error: modelsError } = useCatalog(agent, meta?.projectId ?? null, t)
  const source = useMemo<ImageSource>(
    () => ({ key: `phone:${desktopId}:${chatId}`, chatImage: (file) => api.image(chatId, file), readImage: (path) => api.readImage(chatId, path) }),
    [desktopId, chatId],
  )
  const model = meta?.model || t.model || agent?.defaultModel || ""
  // The input and what's docked over it float on the transcript, which scrolls under them.
  const { stage, tray } = useDock()
  const goals = useGoalControls(meta?.goal, (goal) => st.send(chatId, "", [], { goal }).then(Boolean), {
    running: t.running, waiting: !!(t.questions.length || t.approvals.length), disabled: link !== "connected",
  })
  const asks = t.questions[0] ? { id: t.questions[0].requestId, title: t.questions[0].questions[0]?.question ?? "A question" } : t.approvals[0] ? { id: t.approvals[0].requestId, title: t.approvals[0].title } : null
  // A new request comes up, not lowered.
  useEffect(() => setLowered(false), [asks?.id])

  const composer = (
    <MobileComposer
      agent={meta?.agent}
      floating
      sunk={!!asks}
      running={t.running}
      disabled={link !== "connected"}
      goalHeader={goals.header}
      goalActive={goals.active}
      placeholder={goals.active ? "Write a reply to queue it" : t.running ? "Queue a follow-up" : "Reply"}
      onSend={(text, images, how) => {
        const state = useMobile.getState()
        const running = state.transcripts[chatId]?.running
        const goalActive = state.metas.find((chat) => chat.id === chatId)?.goal?.status === "active"
        // A command never goes into a turn that's running: it waits its turn.
        const mode = running && how === "now" && !text.startsWith("/") ? "steer" : running || goalActive ? "queue" : undefined
        return st.send(chatId, text, images, { mode }).then(Boolean)
      }}
      onGoal={supportsGoals(meta?.agent) ? goals.command : undefined}
      onStop={() => void st.interrupt(chatId)}
      models={models}
      modelsFailed={modelsFailed}
      modelsError={modelsError}
      model={model}
      effort={meta?.effort ?? ""}
      fastMode={meta?.fastMode ?? undefined}
      onModel={(m, e, fast) => void st.setModel(chatId, m, e, fast)}
      onModelsOpen={load}
      commands={commands}
      permission={meta?.permission ?? agent?.permissionMode ?? null}
      onPermission={(m) => st.setPermission(chatId, m)}
      context={t.context}
      onCompact={(instructions) => {
        if (instructions) void api.compact(chatId, instructions, null).catch((e) => toast.error(String(e)))
        else void st.compact(chatId)
      }}
      onClear={() => void st.clear(chatId)}
    />
  )
  const bare = "rounded-none border-0 bg-transparent"

  return (
    <div className="relative h-full bg-background">
      <CwdContext.Provider value={t.cwd ?? meta?.cwd ?? null}>
        <ImageSourceContext.Provider value={source}>
          {/* The transcript runs under the capsule (it starts below it, and scrolls up behind it). */}
          <div ref={stage} className="absolute inset-0" style={{ "--lead": "calc(var(--safe-top) + 56px)" } as CSSProperties}>
            {transcript ? <Transcript transcript={t} jumpTarget={jumpTarget} className="h-full" /> : <div className="grid h-full place-items-center text-[13px] text-muted-foreground">{link === "connected" ? "Loading…" : ""}</div>}
            <div ref={tray} className="absolute inset-x-0 bottom-0 flex max-h-full flex-col">
              {goals.editor}
              <div className="flex min-h-0 flex-col gap-2 overflow-y-auto overscroll-contain px-2.5 pt-1 [&:not(:empty)]:pb-2">
                {meta?.goal && t.plan.length ? <PlanDock items={t.plan} /> : null}
                {dockTasks.length && meta ? <TasksDock agent={meta.agent} tasks={dockTasks} onStop={link === "connected" ? (id) => void st.stopTask(chatId, id) : undefined} /> : null}
                {queued.length ? (
                  <div className="divide-y rounded-[22px] border bg-card">
                    <div className="px-3.5 py-1.5 text-[11.5px] text-muted-foreground">
                      {queued.length} queued{meta?.goal && meta.goal.status !== "complete" ? " · Sends when the goal finishes" : ""}
                    </div>
                    {queued.map((q, i) => (
                      <div key={q.id} className="flex items-center gap-2 py-1 pr-1 pl-3.5 text-[13px]">
                        <span className="text-[11px] text-faint">{i === 0 ? "Next" : "Queued"}</span>
                        <span className="min-w-0 flex-1 truncate">{q.text || `${q.images.length} image${q.images.length > 1 ? "s" : ""}`}</span>
                        {i === 0 ? (
                          <button className="h-8 rounded-full px-3 text-[12px] font-medium active:bg-accent disabled:opacity-50" disabled={link !== "connected"} onClick={() => void st.queueAction(chatId, { action: "send", id: q.id })}>
                            Send now
                          </button>
                        ) : null}
                        <RoundButton label="Remove queued message" className="size-8" disabled={link !== "connected"} onClick={() => void st.queueAction(chatId, { action: "remove", id: q.id })}>
                          <X className="size-4" />
                        </RoundButton>
                      </div>
                    ))}
                  </div>
                ) : null}
              </div>
              {asks ? (
                <WaitingSheet title={asks.title} lowered={lowered} onLowered={setLowered} composer={composer}>
                  {t.questions[0] ? (
                    <QuestionCard className={bare} request={t.questions[0]} onAnswer={(a) => st.answerQuestion(chatId, t.questions[0].requestId, a)} />
                  ) : (
                    <ApprovalCard className={bare} request={t.approvals[0]} queued={t.approvals.length - 1} onAnswer={(d) => st.answer(chatId, t.approvals[0].requestId, d)} />
                  )}
                </WaitingSheet>
              ) : (
                composer
              )}
            </div>
          </div>
        </ImageSourceContext.Provider>
      </CwdContext.Provider>

      <TitleCapsule
        title={meta?.title || "New chat"}
        subtitle={[project?.name, meta ? (agent?.name ?? AGENT_LABEL[meta.agent]) : null].filter(Boolean).join(" · ")}
        actions={
          <div className="flex shrink-0 items-center gap-1">
            {runningSubagents.length > 0 ? (
              <button
                type="button"
                className="flex h-10 shrink-0 items-center gap-1.5 rounded-full bg-background px-3 text-[12.5px] font-medium text-foreground active:bg-accent"
                aria-label={`Subagents: ${subagentSummary(runningSubagents)}`}
                onClick={() => setSubagentsOpen(true)}
              >
                <span className="size-2 shrink-0 rounded-full bg-green animate-pulse" />
                <span className="tabular-nums">{runningSubagents.length}</span>
              </button>
            ) : null}
            <RoundButton label="Chat menu" className="bg-background" onClick={() => setMenu(true)}>
              <MoreHorizontal className="size-5" />
            </RoundButton>
          </div>
        }
      />
      <LinkBanner className="absolute inset-x-3 top-[calc(var(--safe-top)+68px)] z-10" />
      <ChatMenu meta={menu ? (meta ?? null) : null} onClose={() => setMenu(false)} />
      <PullSheet
        open={subagentsOpen && runningSubagents.length > 0}
        onClose={() => setSubagentsOpen(false)}
        title="Running subagents"
        header={<SheetHead title="Running subagents" sub={subagentSummary(runningSubagents)} />}
      >
        <SubagentsPanel
          subagents={runningSubagents}
          open={subagentsOpen}
          stepsById={subagentSteps}
          onStop={link === "connected" ? (id) => void st.stopTask(chatId, id) : undefined}
          onJumpToItem={jumpToItem}
          onClose={() => setSubagentsOpen(false)}
          touch
        />
      </PullSheet>
    </div>
  )
}
