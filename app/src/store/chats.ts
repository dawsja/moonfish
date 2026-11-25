import type { GoalAction } from "@/lib/goals"
// Chats: the list for the sidebar, and a transcript per opened chat, kept current from the
// app's `chat-event` stream.

import { toast } from "sonner"
import { create } from "zustand"
import { emptyTranscript, reduce, replay, type ChatEvent, type ChatMeta, type Decision, type QueueAction, type SendMode, type Transcript, type Waiting } from "@/lib/chat"

export { needsYou, waitingOf } from "@/lib/chat"
import { ipc, onChatEvent, onChatsChanged } from "@/lib/ipc"
import { notify } from "@/lib/notify"
import type { AgentKind, PermissionMode } from "@/lib/types"
import { useApp } from "./app"
import { draftKey, drafts } from "./drafts"

type Image = { mime: string; data: string; name?: string }

type ChatsStore = {
  metas: ChatMeta[]
  transcripts: Record<string, Transcript>
  /** What each running chat waits on the user for, as the app last said (`chat_waiting`):
   *  it knows about chats this window never opened. */
  waiting: Record<string, Waiting>
  /** Chats with background work running, as the app last said (`chat_background`). */
  background: string[]
  /** Chats mid-turn, as the app last said (`chat_busy`): it knows about chats this window
   *  never opened (a phone runs them). */
  busy: string[]
  /** A chat opened to answer it: its waiting card takes the cursor once it shows. */
  focus: string | null

  init(): Promise<void>
  refresh(): Promise<void>
  /** Opens a chat in the main area. */
  open(chatId: string): Promise<void>
  /** Loads the chat's transcript (once), without showing it anywhere. */
  load(chatId: string): Promise<void>
  /** Opens the chat on its approval or question, wherever the window was. */
  answerChat(chatId: string): Promise<void>
  /** Sends a prompt; with no chat yet, creates one in the active project first and shows it.
   *  True once it's sent; false (after saying why) when it wasn't. A new chat whose first
   *  message didn't go is removed again, and the message goes back in the new-chat box. */
  send(
    chatId: string | null,
    text: string,
    opts: { agent: AgentKind; model?: string; effort?: string; fastMode?: boolean; permission: PermissionMode; cwd?: string; images?: Image[]; goal?: GoalAction; mode?: SendMode },
  ): Promise<boolean>
  /** `/compact`: the agent summarizes the conversation so far. */
  compact(chatId: string, instructions: string): Promise<void>
  /** `/clear`: the agent starts a fresh session from the next message. */
  clear(chatId: string): Promise<void>
  sleep(chatId: string): Promise<void>
  interrupt(chatId: string): Promise<void>
  /** Stops one background task. */
  stopTask(chatId: string, taskId: string): Promise<void>
  answer(chatId: string, requestId: string, decision: Decision): Promise<void>
  answerQuestion(chatId: string, requestId: string, answers: Record<string, string[]>): Promise<void>
  setModel(chatId: string, model: string, effort: string, fastMode?: boolean): Promise<void>
  /** Reasoning effort for the chat (empty: the model's default). */
  setEffort(chatId: string, effort: string): Promise<void>
  remove(chatId: string): Promise<void>
  /** Asks first ("Delete … ?"), then removes it. */
  confirmRemove(chatId: string): Promise<void>
  rename(chatId: string, title: string): Promise<void>
  pin(chatId: string, pinned: boolean): Promise<void>
  setPermission(chatId: string, mode: PermissionMode): Promise<void>
  queueAction(chatId: string, command: QueueAction): Promise<boolean>
  /** Sends a queued message now (the first one, without an id). */
  sendQueued(chatId: string, id?: string): Promise<void>
}

let started = false
let refreshRequest = 0

export const useChats = create<ChatsStore>((set, get) => ({
  metas: [],
  transcripts: {},
  waiting: {},
  background: [],
  busy: [],
  focus: null,

  async init() {
    if (started) return
    started = true
    await onChatEvent((chatId, event, seq) => apply(chatId, event, seq))
    // A chat started, renamed or deleted elsewhere (a phone) shows up here too.
    await onChatsChanged(() => void get().refresh())
    await get().refresh()
  },

  async refresh() {
    const request = ++refreshRequest
    const snapshot = await Promise.all([
      ipc.chatList(), ipc.chatWaiting(), ipc.chatBackground(), ipc.chatBusy(),
    ]).catch(() => null)
    if (!snapshot || request !== refreshRequest) return
    const [metas, waiting, background, busy] = snapshot
    const known = new Set(metas.map((m) => m.id))
    const removed = new Set(get().metas.filter((m) => !known.has(m.id)).map((m) => m.id))
    set((s) => ({ metas, waiting, background, busy, transcripts: Object.fromEntries(
      Object.entries(s.transcripts).filter(([id]) => known.has(id)),
    ) }))
    const app = useApp.getState()
    if (app.chatId && removed.has(app.chatId)) app.setActive({ chatId: null })
  },

  async open(chatId) {
    const app = useApp.getState()
    const projectId = get().metas.find((m) => m.id === chatId)?.projectId ?? app.projectId
    app.setActive({ projectId, chatId, viewer: null })
    useApp.setState({ view: "chat" })
    await get().load(chatId)
  },

  async load(chatId) {
    if (get().transcripts[chatId] || loading.has(chatId)) return
    // Events that arrive while the snapshot loads wait, then the ones it doesn't have apply.
    loading.set(chatId, [])
    const snap = await ipc.chatSnapshot(chatId).catch(() => ({ events: [] as ChatEvent[], seq: 0, live: false, busy: false }))
    const held = loading.get(chatId) ?? []
    loading.delete(chatId)
    snapshotSeq.set(chatId, snap.seq)
    const t = held.filter(([seq]) => seq > snap.seq).reduce((t, [, e]) => reduce(t, e), replay(snap.events, snap.busy, snap.live))
    set((s) => ({ transcripts: { ...s.transcripts, [chatId]: t } }))
  },

  async answerChat(chatId) {
    const meta = get().metas.find((m) => m.id === chatId)
    if (!meta) return
    set({ focus: chatId })
    await get().open(chatId)
  },

  async send(chatId, text, opts) {
    const projectId = useApp.getState().projectId
    let id = chatId
    try {
      if (!id) {
        if (!projectId) return false
        const meta = await ipc.chatCreate(projectId, opts.agent, opts.model ?? null, opts.effort ?? null, opts.cwd ?? null, opts.fastMode)
        id = meta.id
        set((s) => ({ metas: [meta, ...s.metas.filter((m) => m.id !== meta.id)], transcripts: { ...s.transcripts, [meta.id]: emptyTranscript() } }))
        useApp.getState().setActive({ chatId: meta.id })
      }
      const chat = id
      const permission = chatId ? null : opts.permission
      if (opts.goal) {
        await ipc.chatGoal(chat, opts.goal, permission)
      } else {
        const images = (opts.images ?? []).map(({ mime, data }) => ({ mime, data }))
        await ipc.chatSend(chat, { id: crypto.randomUUID(), text, images }, permission, opts.mode)
      }
      await get().refresh()
      return true
    } catch (e) {
      const msg = String(e)
      const login = msg.includes("isn't signed in") ? opts.agent : null
      const meta = get().metas.find((m) => m.id === id)
      const project = useApp.getState().settings?.projects.find((p) => p.id === (meta?.projectId ?? projectId))
      const locFocus = project?.location ? `location:${project.location.kind === "local" ? "local" : JSON.stringify([project.location.kind, project.location.kind === "wsl" ? project.location.distro : project.location.host])}` : undefined
      const missing = msg.includes("isn't installed")
      toast.error(
        msg,
        login
          ? { action: { label: "Sign in", onClick: () => useApp.getState().openSettings("providers", locFocus) } }
          : missing
            ? { action: { label: "Providers", onClick: () => useApp.getState().openSettings("providers", locFocus) } }
            : undefined,
      )
      if (id && !chatId) {
        const made = id
        await get().remove(made)
        const cur = useApp.getState()
        if (cur.chatId === null && cur.projectId === projectId) cur.setActive({ draftCwd: opts.cwd ?? null })
        drafts.set(draftKey(null, projectId ?? "", opts.cwd ?? null), text, (opts.images ?? []).map((i) => ({ ...i, name: i.name ?? "image" })))
      }
      return false
    }
  },

  async compact(chatId, instructions) {
    await ipc.chatCompact(chatId, instructions, null).catch((e) => toast.error(String(e)))
  },

  async clear(chatId) {
    await ipc.chatClear(chatId).catch((e) => toast.error(String(e)))
    get().refresh()
  },

  async sleep(chatId) {
    try {
      await ipc.chatSleep(chatId)
      await get().refresh()
    } catch (e) {
      toast.error(String(e))
    }
  },

  async interrupt(chatId) {
    await ipc.chatInterrupt(chatId).catch((e) => toast.error(String(e)))
  },

  async stopTask(chatId, taskId) {
    await ipc.chatStopTask(chatId, taskId).catch((e) => toast.error(String(e)))
  },

  async answer(chatId, requestId, decision) {
    await ipc.chatAnswer(chatId, requestId, decision).catch((e) => toast.error(String(e)))
  },

  async answerQuestion(chatId, requestId, answers) {
    await ipc.chatAnswerQuestion(chatId, requestId, answers).catch((e) => toast.error(String(e)))
  },

  async setModel(chatId, model, effort, fastMode) {
    if (await ipc.chatSetModel(chatId, model, effort, fastMode).catch((e) => (toast.error(String(e)), null))) await get().refresh()
  },

  async setEffort(chatId, effort) {
    if (await ipc.chatSetEffort(chatId, effort).catch((e) => (toast.error(String(e)), null))) await get().refresh()
  },

  async remove(chatId) {
    // A delete that failed leaves the chat listed (it's still there).
    const ok = await ipc.chatDelete(chatId).then(
      () => true,
      (e) => (toast.error(`Couldn't delete the chat: ${e}`), false),
    )
    if (!ok) return
    set((s) => {
      const { [chatId]: _, ...rest } = s.transcripts
      return { metas: s.metas.filter((m) => m.id !== chatId), transcripts: rest }
    })
    drafts.drop(draftKey(chatId, ""))
    if (useApp.getState().chatId === chatId) useApp.getState().setActive({ chatId: null })
  },

  async confirmRemove(chatId) {
    const meta = get().metas.find((m) => m.id === chatId)
    const running = get().transcripts[chatId]?.running || get().busy.includes(chatId)
    const { confirmAction } = await import("@/components/Prompt")
    const ok = await confirmAction({
      title: `Delete ${meta?.title ? `"${meta.title}"` : "this chat"}?`,
      description: `Moonfish permanently deletes its messages and attached images.${running ? " The running agent stops now." : ""}`,
      confirm: "Delete",
      danger: true,
    })
    if (ok) await get().remove(chatId)
  },

  async rename(chatId, title) {
    if (await ipc.chatRename(chatId, title).catch((e) => (toast.error(String(e)), null))) await get().refresh()
  },

  async pin(chatId, pinned) {
    if (await ipc.chatPin(chatId, pinned).catch((e) => (toast.error(String(e)), null))) await get().refresh()
  },

  async setPermission(chatId, mode) {
    try {
      await ipc.chatSetPermission(chatId, mode)
      await get().refresh()
    } catch (e) {
      toast.error(String(e))
    }
  },

  async queueAction(chatId, command) {
    try {
      const changed = await ipc.chatQueue(chatId, command)
      await get().refresh()
      return changed
    } catch (e) {
      toast.error(String(e))
      return false
    }
  },

  async sendQueued(chatId, id) {
    const item = id ?? get().metas.find((m) => m.id === chatId)?.queued?.[0]?.id
    if (item) await get().queueAction(chatId, { action: "send", id: item })
  },
}))

/** Copies the chat as Markdown (to paste into an issue or a PR description). */
export async function copyChatMarkdown(chatId: string) {
  try {
    const md = await ipc.chatMarkdown(chatId)
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager")
    await writeText(md)
    toast.success("Copied the chat as Markdown")
  } catch (e) {
    toast.error(`Couldn't copy the chat: ${e}`)
  }
}

/** Chats whose snapshot is loading, and the events (with their numbers) that came meanwhile. */
const loading = new Map<string, [number, ChatEvent][]>()

/** Batches events per animation frame so streaming text doesn't re-render per token. */
let queue: [string, ChatEvent, number | undefined][] = []
/** Per chat, the number of the last event its snapshot had: an event queued before the
 *  snapshot came back, and already in it, isn't applied twice. */
const snapshotSeq = new Map<string, number>()
let scheduled = false
function apply(chatId: string, event: ChatEvent, seq?: number) {
  notify(event, useChats.getState().metas.find((m) => m.id === chatId))
  const held = loading.get(chatId)
  if (held) {
    if (seq !== undefined) held.push([seq, event])
    return
  }
  queue.push([chatId, event, seq])
  if (scheduled) return
  scheduled = true
  // Once a frame while it's shown; while hidden there are no frames (a minimized window would
  // hold every event), so a timer instead.
  // A frame that never comes (the window was hidden after this was scheduled) is covered too:
  // whichever runs first applies the batch.
  if (!document.hidden) requestAnimationFrame(flushEvents)
  setTimeout(flushEvents, 250)
}

function flushEvents() {
  if (!scheduled) return
  scheduled = false
  const batch = queue
  queue = []
  useChats.setState((s) => {
    const transcripts = { ...s.transcripts }
    // A chat that isn't open here (a phone drives it) loads whole when it's opened.
    for (const [id, e, seq] of batch) {
      if (!transcripts[id] || (seq !== undefined && seq <= (snapshotSeq.get(id) ?? 0))) continue
      transcripts[id] = reduce(transcripts[id], e)
    }
    return { transcripts }
  })
}
