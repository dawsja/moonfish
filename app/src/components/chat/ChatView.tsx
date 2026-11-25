import { useGoalControls } from "./GoalPanel"
import { supportsGoals } from "@/lib/goals"
import { useEffect, useMemo, useRef, useState } from "react"
import { ChevronDown, CornerDownRight, FileText, FolderPlus, Image as ImageIcon, LoaderCircle, MoreHorizontal, Pause, Pencil, Server, Trash2, X } from "lucide-react"
import { AgentGlyph, Mark } from "@/components/icons"
import { promptText } from "@/components/Prompt"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { availableAgents, pickNewChatAgent } from "@/lib/agents"
import { toast } from "sonner"
import { emptyTranscript, type ChatMeta, type ModelOption, type QueuedMessage, type SlashCommand, type Transcript as T } from "@/lib/chat"
import { copyText } from "@/lib/clipboard"
import { LocationIcon } from "@/components/LocationIcon"
import { addLocalFolder, OpenProjectMenu, useDistros } from "@/components/OpenProject"
import { ipc, type Catalog } from "@/lib/ipc"
import { AGENTS, AGENT_LABEL, locationLabel, type AgentKind, type PermissionMode, type Project } from "@/lib/types"
import { useApp, useProject } from "@/store/app"
import { copyChatMarkdown, useChats } from "@/store/chats"
import { draftKey, drafts } from "@/store/drafts"
import { useGit, useScope } from "@/store/git"
import { providerLocationKey, useLocationProbes, useProviders } from "@/store/providers"
import { useStartOpenDistro } from "@/store/wsl"
import { Viewer } from "./Viewer"
import { ApprovalCard } from "./Approval"
import { QuestionCard } from "./QuestionCard"
import { Composer, editInComposer, STEER_KEY } from "./Composer"
import { Tip } from "@/components/Tip"
import { CwdContext } from "./cwd"
import { Transcript, useDock, type JumpTarget } from "./Transcript"
import { isDockTask, TasksDock } from "./TasksDock"
import { PlanDock } from "./PlanDock"
import { SubagentsMenu } from "./SubagentsMenu"
import { SubagentView } from "./SubagentView"
import { extractSubagents, type SubagentItem } from "@/lib/subagents"

/** The active chat, a new chat in the active project, or a file/diff viewer. */
export function ChatView() {
  const settings = useApp((s) => s.settings)
  const chatId = useApp((s) => s.chatId)
  const viewer = useApp((s) => s.viewer)
  const project = useProject()
  const meta = useChats((s) => s.metas.find((m) => m.id === chatId))
  const transcript = useChats((s) => (chatId ? s.transcripts[chatId] : undefined))
  const scope = useScope()

  useEffect(() => {
    if (chatId && !transcript) void useChats.getState().load(chatId)
  }, [chatId, transcript])

  if (!settings) return null
  if (!project) return <NoProject />
  if (viewer && scope) return <Viewer scope={scope} viewer={viewer} />
  // Keyed, so nothing of one chat (how far it's scrolled, an open dock) carries to the next, and
  // a new chat in another project starts from that project's agent and model.
  if (chatId && meta && meta.projectId === project.id) return <OpenChat key={meta.id} project={project} meta={meta} transcript={transcript ?? emptyTranscript()} />
  return <NewChat key={project.id} project={project} />
}

function useAgentDefaults(project: Project, agent: AgentKind) {
  const provider = useApp((s) => s.settings)!.providers[agent]
  return { model: project.defaultModel || provider.defaultModel || "", permission: provider.permissionMode }
}

/** Cache catalogs by agent and project so opening the model or slash menu doesn't
 *  repeatedly start the CLI. Failed requests are retried. */
const catalogCache = new Map<string, { at: number; catalog: Promise<Catalog> }>()
const CATALOG_FRESH = 5 * 60_000

/** Clears cached model/command catalogs so fresh account permissions load after sign-in. */
export function clearCatalogCache(agent?: AgentKind) {
  if (!agent) {
    catalogCache.clear()
    return
  }
  const prefix = `${agent}|`
  for (const key of catalogCache.keys()) {
    if (key.startsWith(prefix)) catalogCache.delete(key)
  }
}
/** The commands a running chat of each agent last listed, so a new chat has them at once. */
const seenCommands = new Map<string, SlashCommand[]>()
function useCatalog(agent: AgentKind, projectId: string, live: { models: ModelOption[]; commands: SlashCommand[] } = { models: [], commands: [] }) {
  const key = `${agent}|${projectId}`
  const [fetched, setFetched] = useState<Catalog>()
  const [error, setError] = useState<string | null>(null)
  const request = useRef(0)
  useEffect(() => {
    setFetched(undefined)
    setError(null)
    return () => { request.current++ }
  }, [key])
  if (live.commands.length) seenCommands.set(key, live.commands)
  const checkSpeed = agent === "claude" || agent === "codex"
  const load = async () => {
    if (!checkSpeed && live.models.length && live.commands.length) return
    const id = ++request.current
    // Recheck speed against the current login, even when replaying an older chat.
    if (checkSpeed || !fetched?.models.length) setFetched(undefined)
    setError(null)
    try {
      let hit = catalogCache.get(key)
      if (!hit || checkSpeed || Date.now() - hit.at > CATALOG_FRESH) {
        hit = { at: Date.now(), catalog: ipc.providerCatalog(agent, projectId) }
        catalogCache.set(key, hit)
      }
      const catalog = await hit.catalog
      if (!catalog.models.length) catalogCache.delete(key)
      if (request.current === id) setFetched(catalog)
    } catch (e) {
      if (request.current === id) {
        catalogCache.delete(key)
        setFetched({ models: [], commands: [] })
        setError(String(e))
      }
    }
  }
  const models = checkSpeed && fetched?.models.length ? fetched.models : live.models.length ? live.models : fetched?.models
  const commands = live.commands.length ? live.commands : (seenCommands.get(key) ?? fetched?.commands ?? [])
  return { models, commands, load, error: live.models.length ? null : error }
}

function NewChat({ project }: { project: Project }) {
  const settings = useApp((s) => s.settings)!
  const listChoices = useProviders((s) => s.choices)
  const locationProbes = useLocationProbes(project.location)
  const locationKey = providerLocationKey(project.location)
  const localBinaries = AGENTS.map((a) => settings.providers[a].binary).join("\n")
  const { distro, starting } = useStartOpenDistro(project.location)
  const locationRef = useRef(project.location)
  locationRef.current = project.location
  const providersRef = useRef(settings.providers)
  providersRef.current = settings.providers

  useEffect(() => {
    if (starting) return
    void useProviders.getState().probeLocation(locationRef.current, providersRef.current)
  }, [locationKey, localBinaries, starting])

  const baseChoices = availableAgents(settings, { choices: listChoices, probes: locationProbes.probes })
  const preferred = project.defaultAgent ?? settings.providers.defaultAgent
  const [pickedAgent, setPickedAgent] = useState<AgentKind | null>(null)
  const agent = pickNewChatAgent(preferred, baseChoices, pickedAgent)
  const choices = availableAgents(settings, { choices: listChoices, probes: locationProbes.probes, selected: agent })
  const d = useAgentDefaults(project, agent)
  const [permissionOverride, setPermissionOverride] = useState<PermissionMode | null>(null)
  const permission = permissionOverride ?? d.permission
  const [model, setModel] = useState<string | null>(null)
  const [effort, setEffort] = useState("")
  const [fastMode, setFastMode] = useState<boolean>()
  const { models, commands, load, error: modelsError } = useCatalog(agent, project.id)
  const draftCwd = useApp((s) => s.draftCwd)
  const draft = draftKey(null, project.id, draftCwd)
  const worktree = useGit((s) => s.worktrees[project.id]?.find((w) => !w.main && w.path === draftCwd))
  const send = (text: string, images: { mime: string; data: string; name?: string }[] = [], _how?: "send" | "steer", permissionOverride?: PermissionMode) =>
    useChats.getState().send(null, text, { agent, model: model ?? undefined, effort: effort || undefined, fastMode, permission: permissionOverride ?? permission, cwd: draftCwd ?? undefined, images })
  const goals = useGoalControls(null, async (goal) => {
    if (goal.action !== "set") {
      toast.info("This chat has no goal yet.")
      return false
    }
    return useChats.getState().send(null, goal.objective, { agent, model: model ?? undefined, effort: effort || undefined, fastMode, permission, cwd: draftCwd ?? undefined, goal })
  })
  // On this computer, whether the chosen agent's CLI is there at all: said before the first
  // message fails, with the way to install it.
  const installed = project.location.kind === "local" ? (locationProbes.probes[agent]?.found ?? null) : null

  return (
    <div className="relative flex h-full min-h-0 min-w-0 flex-col overflow-clip bg-background">
      <div className="flex min-h-0 flex-1 flex-col items-center justify-center px-6 pb-[12vh]">
        <div className="w-full max-w-[46rem]">
          <div className="mb-6 flex items-end gap-3">
            <Mark size={34} />
            <div>
              <h1 className="text-[22px] leading-tight font-semibold tracking-tight">What should we work on?</h1>
              <p className="mt-1 text-[12.5px] text-muted-foreground">
                {AGENT_LABEL[agent]} in <span className="font-medium text-foreground">{project.name}</span>
                {worktree ? (
                  <span className="rounded-full ml-1 border px-1.5 font-mono text-[11px] text-muted-foreground">⎇ {worktree.branch}</span>
                ) : null}
                <span className="text-faint"> · {locationLabel(project.location)}</span>
                {starting ? (
                  <span className="ml-1.5 inline-flex items-center gap-1 text-muted-foreground">
                    <LoaderCircle className="size-3 animate-spin" /> starting {distro}…
                  </span>
                ) : null}
              </p>
              {installed === false ? (
                <p className="mt-1.5 text-[12.5px] text-warn" role="status">
                  {AGENT_LABEL[agent]} isn't installed on this computer.{" "}
                  <button className="underline underline-offset-2 hover:text-foreground" onClick={() => useApp.getState().openSettings("providers")}>
                    Set it up in Providers
                  </button>
                  {choices.length > 1 ? " or pick another agent below." : "."}
                </p>
              ) : null}
            </div>
          </div>
          <Composer
            key={draft}
            draftKey={draft}
            agent={agent}
            agents={choices}
            onAgent={(a) => {
              setPickedAgent(a)
              setModel(null)
              setEffort("")
              setFastMode(undefined)
              // That agent's own default mode, not the last one's (which may be Full access).
              setPermissionOverride(null)
            }}
            model={model ?? d.model}
            effort={effort}
            fastMode={fastMode}
            models={models}
            modelsError={modelsError}
            onModel={(id, e, fast) => {
              setFastMode(fast ?? (id === model ? fastMode : undefined))
              setModel(id)
              setEffort(e)
            }}
            onModelsOpen={load}
            commands={commands}
            permission={permission}
            onPermission={setPermissionOverride}
            onSend={send}
            onGoal={supportsGoals(agent) ? goals.command : undefined}
            autoFocus
          />
          <div className="mt-3 grid grid-cols-3 gap-2 text-[12px]">
            {["Explain how this project is laid out", "Find and fix a failing test", "Review my uncommitted changes"].map((t) => (
              <button key={t} className="rounded-lg border bg-card px-3.5 py-2.5 text-left text-muted-foreground hover:border-ring/60 hover:bg-accent hover:text-foreground" onClick={() => editInComposer(t, [])}>
                {t}
              </button>
            ))}
          </div>
          {goals.editor}
        </div>
      </div>
    </div>
  )
}

function OpenChat({ project, meta, transcript }: { project: Project; meta: ChatMeta; transcript: T }) {
  const agent = meta.agent
  const d = useAgentDefaults(project, agent)
  const { models, commands, load, error: modelsError } = useCatalog(agent, project.id, transcript)
  // The mode it last ran with (from here, a phone) before the agent's default.
  const permission = meta.permission ?? d.permission
  const queued = meta.queued ?? []
  const chats = useChats.getState()
  const dock = useRef<HTMLDivElement>(null)
  const focus = useChats((s) => s.focus === meta.id)
  const card = transcript.questions[0]?.requestId ?? transcript.approvals[0]?.requestId
  const loaded = useChats((s) => !!s.transcripts[meta.id])
  // The input and what's docked over it float on the transcript, which scrolls under them.
  const { stage, tray } = useDock()
  // Opened to answer it (the inbox, a notification): the waiting card takes the cursor.
  useEffect(() => {
    if (!focus) return
    const el = dock.current?.querySelector<HTMLElement>("[data-waiting]")
    if (!el) {
      // Nothing waits any more once the chat loaded: nothing to focus.
      if (loaded) useChats.setState({ focus: null })
      return
    }
    useChats.setState({ focus: null })
    el.scrollIntoView({ block: "nearest" })
    el.querySelector<HTMLElement>("[role=region]")?.focus({ preventScroll: true })
    el.animate([{ boxShadow: "0 0 0 2px var(--primary)" }, { boxShadow: "0 0 0 0 transparent" }], { duration: 1200, easing: "ease-out" })
  }, [focus, card, loaded])

  const goals = useGoalControls(meta.goal, (goal) => chats.send(meta.id, "", { agent, permission, goal }), { running: transcript.running, waiting: !!card })
  const subagents = useMemo(() => extractSubagents(transcript), [transcript])
  const dockTasks = useMemo(() => transcript.tasks.filter(isDockTask), [transcript.tasks])
  const [jumpTarget, setJumpTarget] = useState<JumpTarget | null>(null)
  const [openSubagent, setOpenSubagent] = useState<SubagentItem | null>(null)
  const jumpSeq = useRef(0)
  const jumpToItem = (id: string) => setJumpTarget({ id, seq: ++jumpSeq.current })

  const activeSubagent = useMemo(() => {
    if (!openSubagent) return null
    const id = openSubagent.taskId ?? openSubagent.id
    return subagents.find((s) => (s.taskId ?? s.id) === id) ?? { ...openSubagent, status: "completed" as const }
  }, [openSubagent, subagents])

  return (
    <CwdContext.Provider value={transcript.cwd ?? meta.cwd}>
    {activeSubagent ? (
      <SubagentView
        chatId={meta.id}
        subagent={activeSubagent}
        onBack={() => setOpenSubagent(null)}
        onStop={activeSubagent.taskId ? () => void chats.stopTask(meta.id, activeSubagent.taskId!) : undefined}
      />
    ) : (
    <div className="flex h-full min-h-0 min-w-0 flex-col overflow-clip bg-background">
      <ChatHeader
        meta={meta}
        transcript={transcript}
        subagents={subagents}
        onStopTask={(taskId) => void chats.stopTask(meta.id, taskId)}
        onOpenSubagent={(s) => setOpenSubagent(s)}
        onJumpToItem={jumpToItem}
      />
      <div ref={stage} className="relative min-h-0 flex-1">
        <Transcript transcript={transcript} jumpTarget={jumpTarget} className="h-full" />
        <div ref={tray} className="absolute inset-x-0 bottom-0 flex max-h-[75%] min-h-0 flex-col">
          {meta.sleeping && !transcript.running ? (
            <p className="px-6 py-2 text-center text-[12px] text-muted-foreground">
              {/* Over the transcript, which scrolls on behind it. */}
              <span className="rounded-full bg-background px-2 py-0.5">Sleeping · send a message to resume this session.</span>
            </p>
          ) : null}
          {/* A little wider than the transcript's text, which goes fully behind it. */}
          <div className="mx-auto flex min-h-0 w-full max-w-[46rem] flex-col gap-2 px-3 pb-4">
            <div ref={dock} className="flex min-h-0 flex-col gap-2 overflow-y-auto overscroll-contain empty:hidden">
              {transcript.plan.length ? <PlanDock items={transcript.plan} /> : null}
              {dockTasks.length ? <TasksDock agent={meta.agent} tasks={dockTasks} onStop={(id) => void chats.stopTask(meta.id, id)} /> : null}
              {transcript.questions[0] ? (
                <div data-waiting className="rounded-2xl flex min-h-0 flex-col">
                  <QuestionCard request={transcript.questions[0]} onAnswer={(a) => chats.answerQuestion(meta.id, transcript.questions[0].requestId, a)} />
                </div>
              ) : transcript.approvals[0] ? (
                <div data-waiting className="rounded-2xl flex min-h-0 flex-col">
                  <ApprovalCard request={transcript.approvals[0]} queued={transcript.approvals.length - 1} onAnswer={(dec) => chats.answer(meta.id, transcript.approvals[0].requestId, dec)} />
                </div>
              ) : null}
            </div>
            <div className="shrink-0">
              {goals.editor}
              {queued.length ? <QueueDock chatId={meta.id} items={queued} running={transcript.running} goal={meta.goal} /> : null}
              <Composer
                history={transcript.items.filter((item) => item.kind === "user").map((item) => item.text)}
                key={meta.id}
                draftKey={draftKey(meta.id, project.id)}
                agent={agent}
                model={meta.model || transcript.model || d.model}
                effort={meta.effort}
                fastMode={meta.fastMode ?? undefined}
                context={transcript.context}
                models={models}
                modelsError={modelsError}
                onModel={(id, e, fast) => void chats.setModel(meta.id, id, e, fast)}
                onModelsOpen={load}
                commands={commands}
                onClear={() => chats.clear(meta.id)}
                onGoal={supportsGoals(agent) ? goals.command : undefined}
                goalHeader={goals.header}
                goalActive={goals.active}
                onCompact={(instructions) => chats.compact(meta.id, instructions)}
                permission={permission}
                onPermission={(m) => chats.setPermission(meta.id, m)}
                running={transcript.running}
                onSend={(text, images, how, permissionOverride) => {
                  const state = useChats.getState()
                  const running = state.transcripts[meta.id]?.running
                  const goalActive = state.metas.find((chat) => chat.id === meta.id)?.goal?.status === "active"
                  // A command never goes into a turn that's running: it waits its turn.
                  const mode = running && how === "steer" && !text.startsWith("/") ? "steer" : running || goalActive ? "queue" : undefined
                  return chats.send(meta.id, text, { agent: meta.agent, permission: permissionOverride ?? permission, images, mode })
                }}
                onSteerQueued={queued.length ? () => void chats.sendQueued(meta.id) : undefined}
                onStop={() => chats.interrupt(meta.id)}
                placeholder={goals.active ? "Write a reply to queue it" : transcript.running ? `Queue a reply or press ${STEER_KEY}+Enter to send now` : "Reply. Type @ for files or / for commands."}
                lockAgent
                autoFocus
              />
            </div>
          </div>
        </div>
      </div>
    </div>
    )}
    </CwdContext.Provider>
  )
}

function ChatHeader({
  meta,
  transcript,
  subagents,
  onStopTask,
  onOpenSubagent,
  onJumpToItem,
}: {
  meta: ChatMeta
  transcript: T
  subagents: SubagentItem[]
  onStopTask: (taskId: string) => void
  onOpenSubagent: (subagent: SubagentItem) => void
  onJumpToItem: (itemId: string) => void
}) {
  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b px-4">
      <AgentGlyph agent={meta.agent} className="size-3.5 shrink-0" />
      <span className="truncate text-[13px] font-medium">{meta.title || "New chat"}</span>
      {transcript.running ? <LoaderCircle className="size-3.5 shrink-0 animate-spin text-muted-foreground" /> : null}
      <div className="ml-auto flex shrink-0 items-center gap-2">
        <SubagentsMenu subagents={subagents} onStop={onStopTask} onOpenSubagent={onOpenSubagent} onJumpToItem={onJumpToItem} />
        <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button className="rounded-md grid size-7 place-items-center text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Chat menu">
            <MoreHorizontal className="size-4" />
          </button>
        </DropdownMenuTrigger>
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
      </div>
    </div>
  )
}

/** Messages waiting for the turn to end: each can go now, back to the composer, or away. */
function QueueDock({ chatId, items, running, goal }: { chatId: string; items: QueuedMessage[]; running: boolean; goal: ChatMeta["goal"] }) {
  const chats = useChats.getState()
  const waitsForGoal = goal && goal.status !== "complete"
  const holding = waitsForGoal ? goal.status !== "active" : !running
  return (
    <div className="mx-auto w-[calc(100%-3rem)] max-w-[40rem] rounded-t-lg border border-b-0 bg-card text-[12.5px]">
      <div className="flex h-6 items-center gap-1.5 px-3 pt-1 text-[10.5px] text-muted-foreground">
        {holding ? <Pause className="size-3" /> : null}
        <span className="font-medium text-foreground">{items.length} queued</span>
        <span className="truncate text-faint">{waitsForGoal ? "· Sends when the goal finishes" : running ? "· Sends when the agent finishes" : "· Waiting to send"}</span>
        {holding ? (
          <button className="rounded-md ml-auto px-1.5 text-[11.5px] text-foreground hover:bg-accent" onClick={() => void chats.sendQueued(chatId)}>
            Send next
          </button>
        ) : null}
      </div>
      <ul className="max-h-40 overflow-y-auto pb-1">
        {items.map((q, i) => (
          <li key={q.id} className="group/q flex h-8 items-center gap-2 px-3 hover:bg-accent/50">
            <span className="w-3 shrink-0 text-right font-mono text-[10.5px] text-faint">{i + 1}</span>
            <span className="min-w-0 flex-1 truncate">{q.text.replace(/\s+/g, " ")}</span>
            {q.images.length ? (
              <span className="flex shrink-0 items-center gap-0.5 text-[11px] text-faint">
                <ImageIcon className="size-3" /> {q.images.length}
              </span>
            ) : null}
            <Tip content={running ? `Send at the next agent step. Press ${STEER_KEY}+Enter in an empty message box to send the first queued message.` : "Send now"}>
              <button className="rounded-md flex h-6 shrink-0 items-center gap-1 px-1.5 text-[11.5px] text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => void chats.sendQueued(chatId, q.id)}>
                <CornerDownRight className="size-3" /> Send now
              </button>
            </Tip>
            <Tip content="Move to the message box to edit">
              <button
                className="rounded-md grid size-6 shrink-0 place-items-center text-muted-foreground hover:bg-accent hover:text-foreground"
                aria-label="Edit queued message"
                onClick={async () => {
                  try {
                    const images = await Promise.all(q.images.map(async (name) => {
                      const url = await ipc.chatImage(chatId, name)
                      const [head, data] = url.split(";base64,")
                      return { mime: head.slice("data:".length), data, name }
                    }))
                    if (!await chats.queueAction(chatId, { action: "remove", id: q.id })) return
                    if (useApp.getState().chatId === chatId) editInComposer(q.text, images)
                    else if (useChats.getState().metas.some((m) => m.id === chatId)) {
                      const key = draftKey(chatId, "")
                      const draft = drafts.get(key)
                      drafts.set(key, draft.text.trim() ? `${q.text}\n\n${draft.text}` : q.text, [...images, ...draft.images].slice(0, 8))
                    }
                  } catch (e) {
                    toast.error(String(e))
                  }
                }}
              >
                <Pencil className="size-3" />
              </button>
            </Tip>
            <Tip content="Remove">
              <button className="rounded-md grid size-6 shrink-0 place-items-center text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Remove queued message" onClick={() => void chats.queueAction(chatId, { action: "remove", id: q.id })}>
                <X className="size-3.5" />
              </button>
            </Tip>
          </li>
        ))}
      </ul>
    </div>
  )
}

function NoProject() {
  const openSettings = useApp((s) => s.openSettings)
  const { distros } = useDistros()
  return (
    <div className="flex h-full flex-col items-center justify-center bg-background px-6 pb-[10vh]">
      <Mark size={48} label="Moonfish" />
      <h1 className="mt-5 text-[22px] font-semibold tracking-tight">Moonfish</h1>
      <p className="mt-2 max-w-md text-center text-[13px] leading-relaxed text-muted-foreground">
        Chat with Claude Code, Codex, and Antigravity in projects on this computer, in WSL, or on an SSH host. Add a project to start.
      </p>
      <div className="mt-6 flex gap-2">
        <Button onClick={() => void addLocalFolder()}>
          <FolderPlus /> Open folder
        </Button>
        {distros?.length ? (
          <OpenProjectMenu>
            <Button variant="outline">
              <LocationIcon location={{ kind: "wsl", distro: "" }} /> Folder in WSL <ChevronDown />
            </Button>
          </OpenProjectMenu>
        ) : null}
        <Button variant="outline" onClick={() => openSettings("ssh")}>
          <Server /> Folder on an SSH host
        </Button>
      </div>
    </div>
  )
}
