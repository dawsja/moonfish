// A new chat: where (a project), with whom (an agent), and the first message, which creates it.
// From the home screen's New chat button it's a sheet; `#/new/<projectId>` is the same as a screen.

import { useState } from "react"
import { useGoalControls } from "@/components/chat/GoalPanel"
import { LocationIcon } from "@/components/LocationIcon"
import { AgentGlyph } from "@/components/icons"
import { mobileAvailableAgents } from "@/lib/agents"
import { supportsGoals } from "@/lib/goals"
import { locationLabel, type PermissionMode } from "@/lib/types"
import { cn } from "@/lib/utils"
import { MobileComposer } from "./Composer"
import { useCatalog } from "./models"
import { go } from "./route"
import { PullSheet, SheetHead } from "./sheet"
import { useMobile } from "./store"
import { LinkBanner, TitleCapsule, UNDER_CAPSULE } from "./ui"

/** The agent to work with, and the first message. `sheet`: inside the New chat sheet (the box
 *  sinks into it). `onCreated`: before the new chat opens. */
function NewChatForm({ projectId, sheet, onCreated }: { projectId: string; sheet?: boolean; onCreated?: () => void }) {
  const project = useMobile((s) => s.projects.find((p) => p.id === projectId))
  const allAgents = useMobile((s) => s.agents)
  const agents = mobileAvailableAgents(allAgents, project?.location)
  const defaultAgent = useMobile((s) => s.defaultAgent)
  const link = useMobile((s) => s.link)
  const preferred = project?.defaultAgent ?? defaultAgent
  const [pick, setPick] = useState<string | null>(null)
  const agent = agents.find((a) => a.kind === pick) ?? agents.find((a) => a.kind === preferred) ?? agents[0]
  const [model, setModel] = useState("")
  const [effort, setEffort] = useState("")
  const [fastMode, setFastMode] = useState<boolean>()
  // None picked: the agent's own default on the desktop.
  const [permission, setPermission] = useState<PermissionMode | null>(null)
  const { models, commands, load, failed: modelsFailed, error: modelsError } = useCatalog(agent, projectId)
  const [sending, setSending] = useState(false)
  const opened = (id: string | null) => {
    if (!id) return false
    onCreated?.()
    go(`/chat/${encodeURIComponent(id)}`, true)
    return true
  }
  const goals = useGoalControls(null, async (goal) => {
    if (goal.action !== "set") return false
    return opened(await useMobile.getState().send(null, goal.objective, [], { projectId, agent, model, effort, fastMode, permission: permission ?? undefined, goal }))
  })

  return (
    <>
      {sheet ? <div className="mb-2 px-4 text-[12.5px] font-medium text-muted-foreground">Agent</div> : null}
      <div className={cn("flex shrink-0 gap-1.5 overflow-x-auto", sheet ? "px-3 pb-3" : "px-1 pb-2")} role="group" aria-label="Agent">
        {agents.map((a) => (
          <button
            key={a.kind}
            aria-pressed={agent?.kind === a.kind}
            className={cn("flex h-10 shrink-0 items-center gap-2 rounded-full pr-4 pl-3 text-[13.5px]", agent?.kind === a.kind ? "bg-primary font-semibold text-primary-foreground" : "bg-muted active:bg-accent")}
            onClick={() => (setPick(a.kind), setModel(""), setEffort(""), setFastMode(undefined), setPermission(null))}
          >
            <AgentGlyph agent={a.kind} className="size-4" />
            {a.name}
          </button>
        ))}
        {agents.length === 0 ? <div className="px-1 text-[13px] text-muted-foreground">Enable an agent in Providers on the computer.</div> : null}
      </div>
      {sheet ? null : (
        <p className="px-2 text-[12.5px] leading-relaxed text-muted-foreground">
          It works in {project ? <span className="font-mono text-foreground">{project.path}</span> : "the project"} on the desktop. The chat shows up there too.
        </p>
      )}
      {sheet ? <div className="px-0.5">{goals.editor}</div> : null}
      <div className={sheet ? "" : "mt-auto"}>
        {sheet ? null : goals.editor}
        <MobileComposer
          agent={agent?.kind}
          floating
          sunk={sheet}
          running={false}
          disabled={link !== "connected" || !agent || sending}
          placeholder={agent ? `What should ${agent.name} do?` : "Send a message"}
          onSend={async (text, images, _how, permissionOverride) => {
            setSending(true)
            const id = await useMobile.getState().send(null, text, images, { projectId, agent, model, effort, fastMode, permission: permissionOverride ?? permission ?? undefined })
            setSending(false)
            return opened(id)
          }}
          onGoal={supportsGoals(agent?.kind) ? goals.command : undefined}
          models={models}
          modelsFailed={modelsFailed}
          modelsError={modelsError}
          model={model || agent?.defaultModel || ""}
          effort={effort}
          fastMode={fastMode}
          onModel={(m, e, fast) => {
            setFastMode(fast ?? (m === model ? fastMode : undefined))
            setModel(m)
            setEffort(e)
          }}
          onModelsOpen={load}
          commands={commands}
          permission={permission ?? agent?.permissionMode ?? null}
          onPermission={setPermission}
        />
      </div>
    </>
  )
}

/** `#/new/<projectId>`: a new chat as a screen. */
export function NewChatScreen({ projectId }: { projectId: string }) {
  const project = useMobile((s) => s.projects.find((p) => p.id === projectId))
  return (
    <div className="relative flex h-full flex-col bg-background">
      <div className={cn("flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-2", UNDER_CAPSULE)}>
        <LinkBanner className="mx-1" />
        <NewChatForm projectId={projectId} />
      </div>
      <TitleCapsule title="New chat" subtitle={project?.name} />
    </div>
  )
}

/** New chat from the home screen: pick the project (`initial` first), the agent, and write. */
export function NewChatSheet({ open, initial, onClose }: { open: boolean; initial: string | null; onClose: () => void }) {
  const projects = useMobile((s) => s.projects)
  const [picked, setPicked] = useState<string | null>(null)
  const projectId = (picked && projects.some((p) => p.id === picked) ? picked : null) ?? initial ?? projects[0]?.id ?? null
  const close = () => {
    setPicked(null)
    onClose()
  }
  return (
    <PullSheet open={open} onClose={close} title="New chat" header={<SheetHead title="New chat" />} className="pb-0">
      <div className="min-h-0 shrink overflow-y-auto px-3 pb-3">
        <div className="mb-2 px-1 text-[12.5px] font-medium text-muted-foreground">Project</div>
        <div className="flex flex-col gap-1" role="radiogroup" aria-label="Project">
          {projects.map((p) => {
            const on = p.id === projectId
            return (
              <button key={p.id} role="radio" aria-checked={on} className={cn("flex items-center gap-3 rounded-[22px] p-2.5 text-left active:bg-accent", on && "bg-muted ring-2 ring-primary")} onClick={() => setPicked(p.id)}>
                <span className={cn("grid size-10 shrink-0 place-items-center rounded-full", on ? "bg-background" : "bg-muted")}>
                  <LocationIcon location={p.location} className="size-[18px]" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[15px] font-medium">{p.name}</span>
                  <span className="block truncate font-mono text-[11.5px] text-muted-foreground">{p.location.kind === "local" ? p.path : `${locationLabel(p.location)}:${p.path}`}</span>
                </span>
              </button>
            )
          })}
        </div>
        {projects.length === 0 ? <p className="px-1 text-[13px] text-muted-foreground">No projects on this desktop yet. Open a folder in Moonfish there first.</p> : null}
      </div>
      {projectId ? <NewChatForm key={projectId} projectId={projectId} sheet onCreated={close} /> : null}
    </PullSheet>
  )
}
