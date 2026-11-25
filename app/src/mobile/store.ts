import type { GoalAction } from "@/lib/goals"
// The phone app's state: paired desktops, the link to the one in use, its projects and chats,
// and the open chat's transcript (folded with the desktop's own `reduce`).

import { toast } from "sonner"
import { create } from "zustand"
import { reduce, replay, type ApprovalRequest, type ChatEvent, type ChatMeta, type Decision, type QuestionRequest, type QueueAction, type SendMode, type Transcript } from "@/lib/chat"
import type { AgentKind, PermissionMode, Project } from "@/lib/types"
import { go } from "./route"
import { api, native, phoneName, type AgentInfo, type AlertOpen, type Catalog, type Desktop, type Image, type LinkEvent, type LinkState, type Push, type Waiting } from "./rpc"

type Store = {
  desktops: Desktop[]
  loaded: boolean
  desktopId: string | null
  link: LinkState
  linkError: string | null
  desktopName: string
  projects: Project[]
  /** The desktop has answered with its list at least once (it may have no projects). */
  listed: boolean
  agents: AgentInfo[]
  defaultAgent: AgentKind
  metas: ChatMeta[]
  /** Chats with their agent running on the desktop, and those mid-turn. */
  running: string[]
  busy: string[]
  /** Chats waiting for an answer from you, and for what. */
  waiting: Record<string, Waiting>
  /** The chat open on screen. */
  chatId: string | null
  /** What each waiting chat asks, read for the home screen's cards (`peek`). */
  requests: Record<string, { approval?: ApprovalRequest; question?: QuestionRequest }>
  transcripts: Record<string, Transcript>
  catalogs: Record<string, Catalog>

  init(): Promise<void>
  pair(o: { addresses: string[]; code: string; name?: string }): Promise<Desktop>
  /** Starts the demo desktop (inside the app) and lists it. */
  startDemo(): Promise<Desktop>
  forget(id: string): Promise<void>
  connect(id: string): Promise<void>
  refresh(): Promise<void>
  open(chatId: string): Promise<void>
  leave(): void
  /** Reads what a waiting chat asks (its approval or question) without opening it. */
  peek(chatId: string): Promise<void>
  /** Sends a prompt; with no chat yet (`chatId` null), creates one in `projectId` first. */
  /** The chat it went to, or null (after saying why) when it didn't go. A new chat whose
   *  first message didn't go is removed again. */
  send(
    chatId: string | null,
    text: string,
    images: Image[],
    o?: { projectId?: string; agent?: AgentInfo; model?: string; effort?: string; fastMode?: boolean; permission?: PermissionMode; goal?: GoalAction; mode?: SendMode },
  ): Promise<string | null>
  queueAction(chatId: string, command: QueueAction): Promise<boolean>
  stopTask(chatId: string, taskId: string): Promise<void>
  interrupt(chatId: string): Promise<void>
  answer(chatId: string, requestId: string, decision: Decision): Promise<void>
  answerQuestion(chatId: string, requestId: string, answers: Record<string, string[]>): Promise<void>
  setModel(chatId: string, model: string, effort: string, fastMode?: boolean): Promise<void>
  setPermission(chatId: string, mode: PermissionMode): Promise<void>
  compact(chatId: string): Promise<void>
  clear(chatId: string): Promise<void>
  rename(chatId: string, title: string): Promise<void>
  /** Ends the agent's process; the next message resumes the session. */
  sleep(chatId: string): Promise<void>
  remove(chatId: string): Promise<void>
  catalog(agent: AgentInfo, projectId: string | null): Promise<{ catalog: Catalog | null; error: string | null }>
}

const LAST = "moonfish.mobile.desktop"
const fail = (e: unknown) => toast.error(String(e))

let refreshRequest = 0
/** Chats being peeked at, and the requests answered from here (until the desktop says). */
const peeking = new Set<string>()
export const answered = new Set<string>()
let connection = 0

export const useMobile = create<Store>((set, get) => ({
  desktops: [],
  loaded: false,
  desktopId: null,
  link: "idle",
  linkError: null,
  desktopName: "",
  projects: [],
  listed: false,
  agents: [],
  defaultAgent: "claude",
  metas: [],
  running: [],
  busy: [],
  waiting: {},
  chatId: null,
  requests: {},
  transcripts: {},
  catalogs: {},

  async init() {
    const desktops = await native.desktops().catch(() => [] as Desktop[])
    set({ desktops, loaded: true })
    // Asked to start in the demo, or on a screen (screenshots, tests driving the real app).
    const launch = await native.launch().catch(() => null)
    // Started by tapping a notification (Settings → Alerts): its chat, on its desktop.
    const alert = await native.alertsTakeOpen().catch(() => null)
    const last = localStorage.getItem(LAST)
    const demo = launch?.demo ? await get().startDemo().catch(() => null) : null
    const pick = demo ?? desktops.find((d) => d.id === alert?.desktopId) ?? desktops.find((d) => d.id === last) ?? (desktops.length === 1 ? desktops[0] : null)
    if (pick) await get().connect(pick.id)
    if (launch?.route) go(launch.route, true)
    else if (alert && pick?.id === alert.desktopId) go(chatPath(alert.chatId), true)
    // Tapped while the app runs.
    void (async () => {
      const { listen } = await import("@tauri-apps/api/event")
      await listen("alerts-open", async () => {
        const open = await native.alertsTakeOpen().catch(() => null)
        if (open) await openAlert(open)
      })
    })().catch(() => {})
    // Back from the background: check the link (the phone may have slept through it dropping).
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible" && get().desktopId) void native.wake().catch(() => {})
    })
  },

  async pair(o) {
    const d = await native.pair({ ...o, name: o.name || phoneName() })
    set((s) => ({ desktops: [d, ...s.desktops.filter((x) => x.id !== d.id)] }))
    return d
  },

  async startDemo() {
    const d = await native.demo()
    set((s) => ({ desktops: [...s.desktops.filter((x) => x.id !== d.id), d] }))
    return d
  },

  async forget(id) {
    await native.forget(id)
    if (localStorage.getItem(LAST) === id) localStorage.removeItem(LAST)
    localStorage.removeItem(cacheKey(id))
    set((s) => ({ desktops: s.desktops.filter((d) => d.id !== id), ...(s.desktopId === id ? { desktopId: null, link: "idle" as const, metas: [], projects: [], transcripts: {} } : {}) }))
  },

  async connect(id) {
    if (get().desktopId === id && get().link !== "idle" && get().link !== "revoked") return
    const d = get().desktops.find((x) => x.id === id)
    // The demo is gone when the app closes: not somewhere to come back to.
    if (!d?.demo) localStorage.setItem(LAST, id)
    // The last list seen from this desktop shows (read-only) until it answers.
    set({ desktopId: id, link: "connecting", linkError: null, desktopName: d?.name ?? "", requests: {}, chatId: null, transcripts: {}, catalogs: {}, running: [], busy: [], waiting: {}, listed: false, ...cached(id) })
    const current = ++connection
    resetEvents()
    await native.connect(id, (event) => {
      if (connection === current && get().desktopId === id) onLink(event)
    }).catch((e) => {
      if (connection === current && get().desktopId === id) set({ link: "offline", linkError: String(e) })
    })
  },

  async refresh() {
    if (get().link !== "connected") return
    const request = ++refreshRequest
    const id = get().desktopId
    const isCurrent = () => request === refreshRequest && id === get().desktopId && get().link === "connected"
    const snapshot = await Promise.all([api.chats(), api.projects(), api.agents()]).catch((e) => {
      if (isCurrent()) set({ linkError: String(e) })
      return null
    })
    if (!snapshot || !isCurrent()) return
    const [chats, projects, agents] = snapshot
    const next = {
      metas: [...chats.metas].sort((a, b) => b.updatedAt - a.updatedAt),
      projects,
      agents: agents.agents,
      defaultAgent: agents.defaultAgent,
    }
    const known = new Set(next.metas.map((m) => m.id))
    const waiting = chats.waiting ?? {}
    set((s) => ({ ...next, running: chats.running, busy: chats.busy, waiting, listed: true,
      transcripts: Object.fromEntries(Object.entries(s.transcripts).filter(([id]) => known.has(id))),
      requests: Object.fromEntries(Object.entries(s.requests).filter(([id]) => waiting[id])),
    }))
    if (id && !get().desktops.find((d) => d.id === id)?.demo) cache(id, next)
  },

  async open(chatId) {
    const prev = get().chatId
    if (prev && prev !== chatId) void api.close(prev).catch(() => {})
    set({ chatId })
    await subscribeAndLoadChat(chatId, () => get().chatId === chatId)
  },

  leave() {
    const id = get().chatId
    if (id) void api.close(id).catch(() => {})
    set({ chatId: null })
  },

  async peek(chatId) {
    if (get().link !== "connected" || peeking.has(chatId) || get().chatId === chatId) return
    peeking.add(chatId)
    const desktop = get().desktopId
    try {
      const snap = await api.open(chatId)
      if (get().desktopId !== desktop) return
      const t = replay(snap.events, snap.busy, snap.live)
      set((s) => ({ requests: { ...s.requests, [chatId]: { approval: t.approvals[0], question: t.questions[0] } } }))
    } catch {
      // The card still opens the chat.
    } finally {
      peeking.delete(chatId)
      // Opened meanwhile: it's on screen now, and its subscription is the screen's.
      if (get().chatId !== chatId && get().desktopId === desktop) void api.close(chatId).catch(() => {})
    }
  },

  async send(chatId, text, images, o = {}) {
    let id = chatId
    try {
      if (!id) {
        if (!o.projectId || !o.agent) return null
        let meta = await api.create(o.projectId, o.agent.kind, o.model || null, o.fastMode)
        id = meta.id
        if (o.effort) meta = await api.setEffort(meta.id, o.effort)
        // Open it before sending, so every event of the first turn arrives here.
        const snap = await api.open(meta.id)
        set((s) => ({ metas: [{ ...meta, permission: o.permission ?? meta.permission }, ...s.metas.filter((m) => m.id !== meta.id)], transcripts: { ...s.transcripts, [meta.id]: replay(snap.events, snap.busy, snap.live) }, chatId: meta.id }))
      }
      const chat = id
      const permission = chatId ? null : o.permission ?? null
      if (o.goal) {
        await api.goal(chat, o.goal, permission)
      } else {
        await api.send(chat, text, images, permission, o.mode)
      }
      if (chatId) await get().refresh()
      return chat
    } catch (e) {
      fail(e)
      if (id && !chatId) {
        await get().remove(id)
        set({ chatId: null })
      }
      return null
    }
  },

  async queueAction(chatId, command) {
    try {
      const changed = await api.queue(chatId, command)
      await get().refresh()
      return changed
    } catch (e) {
      fail(e)
      return false
    }
  },

  async stopTask(chatId, taskId) {
    await api.stopTask(chatId, taskId).catch(fail)
  },

  async interrupt(chatId) {
    await api.interrupt(chatId).catch(fail)
  },
  async answer(chatId, requestId, decision) {
    answered.add(requestId)
    await api.answer(chatId, requestId, decision).catch((e) => (answered.delete(requestId), fail(e)))
  },
  async answerQuestion(chatId, requestId, answers) {
    answered.add(requestId)
    await api.answerQuestion(chatId, requestId, answers).catch((e) => (answered.delete(requestId), fail(e)))
  },

  async setModel(chatId, model, effort, fastMode) {
    const meta = await api.setModel(chatId, model, effort, fastMode).catch((e) => (fail(e), null))
    if (meta) await get().refresh()
  },

  async setPermission(chatId, mode) {
    try {
      await api.setPermission(chatId, mode)
      await get().refresh()
    } catch (e) {
      fail(e)
    }
  },

  async compact(chatId) {
    await api.compact(chatId, "", null).catch(fail)
  },
  async clear(chatId) {
    await api.clear(chatId).catch(fail)
  },

  async rename(chatId, title) {
    const next = await api.rename(chatId, title).catch((e) => (fail(e), null))
    if (next) await get().refresh()
  },
  async sleep(chatId) {
    // The desktop announces the change, and the list reloads with the chat asleep.
    await api.sleep(chatId).catch(fail)
  },
  async remove(chatId) {
    const ok = await api.remove(chatId).then(() => true, (e) => (fail(e), false))
    if (!ok) return
    set((s) => {
      const transcripts = { ...s.transcripts }
      delete transcripts[chatId]
      return { metas: s.metas.filter((m) => m.id !== chatId), transcripts }
    })
  },

  async catalog(agent, projectId) {
    const key = `${agent.kind}:${projectId ?? ""}`
    const cachedCatalog = get().catalogs[key]
    if (agent.kind !== "claude" && agent.kind !== "codex" && cachedCatalog?.models.length) {
      return { catalog: cachedCatalog, error: null }
    }
    try {
      const c = await api.catalog(agent.kind, projectId)
      if (c.models.length) set((s) => ({ catalogs: { ...s.catalogs, [key]: c } }))
      return { catalog: c, error: null }
    } catch (e) {
      return { catalog: null, error: String(e) }
    }
  },
}))

const chatPath = (chatId: string) => `/chat/${encodeURIComponent(chatId)}`

/** A notification was tapped: its chat, on its desktop (connecting to that one first). A desktop
 *  no longer paired leaves the screen as it is. */
export async function openAlert(open: AlertOpen) {
  const s = useMobile.getState()
  if (!s.desktops.some((d) => d.id === open.desktopId)) return
  if (s.desktopId !== open.desktopId) await s.connect(open.desktopId)
  go(chatPath(open.chatId))
}

type Cached = Pick<Store, "metas" | "projects" | "agents" | "defaultAgent">
const cacheKey = (id: string) => `moonfish.mobile.cache.${id}`
function cache(id: string, c: Cached) {
  try {
    localStorage.setItem(cacheKey(id), JSON.stringify({ ...c, metas: c.metas.slice(0, 200) }))
  } catch {
    // Full storage: the list just won't show offline.
  }
}
function cached(id: string): Cached {
  try {
    const c = JSON.parse(localStorage.getItem(cacheKey(id)) ?? "null") as Cached | null
    if (c?.metas && c.projects) return c
  } catch {
    // A damaged cache is the same as none.
  }
  return { metas: [], projects: [], agents: [], defaultAgent: "claude" }
}

function onLink(e: LinkEvent) {
  if (e.kind === "push") return onPush(e.push)
  const s = useMobile.getState()
  if (e.state === "connected") {
    resetEvents()
    useMobile.setState({ link: "connected", linkError: null, desktopName: e.desktopName || s.desktopName })
    void s.refresh()
    // Reopen the chat on screen: whatever happened while the link was down comes with it.
    if (s.chatId) void s.open(s.chatId)
  } else if (e.state === "revoked") {
    useMobile.setState({ link: "revoked", linkError: e.error ?? null })
  } else {
    useMobile.setState({ link: e.state, linkError: e.error ?? s.linkError })
  }
}

function onPush(p: Push) {
  const s = useMobile.getState()
  if (p.push === "chats") return void s.refresh()
  if (p.push === "resync") {
    resetEvents()
    void s.refresh()
    if (s.chatId) void s.open(s.chatId)
    return
  }
  if (p.push === "chat") push(p.chatId, p.event, p.seq)
}

async function subscribeAndLoadChat(chatId: string, isStillSelected: () => boolean) {
  const get = useMobile.getState
  const set = useMobile.setState
  if (get().link !== "connected") return
  // The snapshot (an answer) and live events (pushes) arrive by different ways, in either
  // order: events wait until it's in, and the ones it already has are dropped.
  const held: [number | undefined, ChatEvent][] = []
  const desktop = get().desktopId
  loading.set(chatId, held)
  try {
    const snap = await api.open(chatId)
    if (loading.get(chatId) !== held) return
    loading.delete(chatId)
    if (!isStillSelected() || get().desktopId !== desktop || get().link !== "connected") return
    snapshotSeq.set(chatId, snap.seq)
    const t = held.filter(([seq]) => seq === undefined || seq > snap.seq).reduce((t, [, e]) => reduce(t, e), replay(snap.events, snap.busy, snap.live))
    set((s) => ({ transcripts: { ...s.transcripts, [chatId]: t } }))
  } catch (e) {
    if (loading.get(chatId) !== held) return
    loading.delete(chatId)
    if (!isStillSelected() || get().desktopId !== desktop) return
    fail(e)
  }
}

/** Chats whose snapshot is loading, and the events (with their numbers) that came meanwhile. */
const loading = new Map<string, [number | undefined, ChatEvent][]>()
/** Per chat, the last event number its snapshot had. */
const snapshotSeq = new Map<string, number>()

/** Batches events per animation frame so streaming text doesn't re-render per token. */
let queue: [string, ChatEvent, number | undefined][] = []
let scheduled = false
function push(chatId: string, event: ChatEvent, seq?: number) {
  if (event.type === "permissionChanged" || event.type === "goalChanged") void useMobile.getState().refresh()
  const held = loading.get(chatId)
  if (held) {
    held.push([seq, event])
    return
  }
  queue.push([chatId, event, seq])
  if (scheduled) return
  scheduled = true
  requestAnimationFrame(() => {
    scheduled = false
    const batch = queue
    queue = []
    useMobile.setState((s) => {
      const transcripts = { ...s.transcripts }
      for (const [id, e, seq] of batch) {
        if (!transcripts[id] || (seq !== undefined && seq <= (snapshotSeq.get(id) ?? 0))) continue
        transcripts[id] = reduce(transcripts[id], e)
      }
      return { transcripts }
    })
  })
}

function resetEvents() {
  loading.clear()
  snapshotSeq.clear()
  queue = []
}
