import type { GoalAction } from "@/lib/goals"
import type { ChatEvent, ChatMeta, Decision, ModelOption, QueueAction, SendMode, SlashCommand, Snapshot } from "@/lib/chat"
import type { AccountUsage, AgentKind, Limits, PermissionMode, Project } from "@/lib/types"

/** A desktop this phone paired with. `id` is its public key. `demo`: the one inside the app,
 *  there to try the app without a computer (never saved). */
export type Desktop = { id: string; name: string; addresses: string[]; deviceId: string; pairedAt: number; lastAddress: string | null; demo?: boolean }

export type LinkState = "idle" | "connecting" | "connected" | "offline" | "revoked"
export type LinkEvent =
  | { kind: "state"; state: Exclude<LinkState, "idle">; error?: string; address?: string; desktopName?: string }
  | { kind: "push"; push: Push }
/** `seq`: the event's number in its chat (absent from desktops before it was sent). */
export type Push = { push: "chat"; chatId: string; seq?: number; event: ChatEvent } | { push: "chats" } | { push: "resync" }

/** What a chat waits for from you. */
export type Waiting = "approval" | "question" | "plan"

/** An agent the desktop has turned on. Optional availability fields are respected when present. */
export type AgentInfo = {
  kind: AgentKind
  name: string
  defaultModel: string
  permissionMode: PermissionMode
  enabled?: boolean
  available?: boolean
  installed?: boolean
}
export type Catalog = { models: ModelOption[]; commands: SlashCommand[] }
export type Image = { mime: string; data: string }

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core")
  return invoke<T>(cmd, args)
}

/** Settings → Alerts. `permission`: whether Android shows Moonfish's notifications (`prompt`: it
 *  can still ask; `denied`: only its settings can change that; `unsupported`: not the app). */
export type AlertsState = { enabled: boolean; done: boolean; permission: "granted" | "prompt" | "denied" | "unsupported" }
/** A chat a notification was tapped to open. */
export type AlertOpen = { chatId: string; desktopId: string }

export type NearbyDesktop = { name: string; address: string }
export const native = {
  discover: () => invoke<NearbyDesktop[]>("discover"),
  requestPairing: (address: string, name: string) => invoke<void>("request_pairing", { address, name }),
  desktops: () => invoke<Desktop[]>("desktops"),
  /** Starts the demo desktop inside the app and pairs with it. */
  demo: () => invoke<Desktop>("demo_start"),
  /** How the app was asked to start (screenshots and tests): in the demo, on a screen. */
  launch: () => invoke<{ demo: boolean; route: string | null }>("launch"),
  /** With the address and code the desktop shows. */
  pair: (o: { addresses: string[]; code: string; name: string }) => invoke<Desktop>("pair", { addresses: o.addresses, code: o.code, name: o.name }),
  forget: (id: string) => invoke<void>("forget", { id }),
  async connect(id: string, onEvent: (e: LinkEvent) => void) {
    const { Channel } = await import("@tauri-apps/api/core")
    const ch = new Channel<LinkEvent>()
    ch.onmessage = onEvent
    return invoke<void>("connect", { id, onEvent: ch })
  },
  disconnect: () => invoke<void>("disconnect"),
  wake: () => invoke<void>("wake"),
  alertsState: () => invoke<AlertsState>("alerts_state"),
  /** Turning alerts on asks Android for notifications first; they stay off if it says no. */
  alertsSet: (o: { enabled?: boolean; done?: boolean }) => invoke<AlertsState>("alerts_set", o),
  /** Android's notification settings for Moonfish. */
  alertsSettings: () => invoke<void>("alerts_settings"),
  /** The chat a notification was tapped to open, once. */
  alertsTakeOpen: () => invoke<AlertOpen | null>("alerts_take_open"),
  /** The chat is on screen: its notification goes. */
  alertsSeen: (chatId: string) => invoke<void>("alerts_seen", { chatId }),
}

/** A light tap under the finger. */
export function tap() {
  navigator.vibrate?.(10)
}

const rpc = <T>(method: string, params: Record<string, unknown> = {}) => invoke<T>("rpc", { method, params })

export type LimitsResponse = Limits & { accounts?: AccountUsage[] }

export const api = {
  limits: (kind: AgentKind) => rpc<LimitsResponse>("provider.limits", { kind }),
  projects: () => rpc<Project[]>("projects.list"),
  agents: () => rpc<{ agents: AgentInfo[]; defaultAgent: AgentKind }>("agents.list"),
  chats: () => rpc<{ metas: ChatMeta[]; running: string[]; busy: string[]; waiting?: Record<string, Waiting> }>("chats.list"),
  open: (chatId: string) => rpc<Snapshot>("chat.open", { chatId }),
  close: (chatId: string) => rpc<void>("chat.close", { chatId }),
  create: (projectId: string, agent: AgentKind, model: string | null, fastMode?: boolean) => rpc<ChatMeta>("chat.create", { projectId, agent, model, fastMode }),
  send: (chatId: string, text: string, images: Image[], permission: PermissionMode | null, mode?: SendMode) =>
    rpc<void>(mode === "queue" ? "chat.enqueue" : "chat.send", { chatId, prompt: { id: crypto.randomUUID(), text, images }, permission, mode }),
  queue: (chatId: string, command: QueueAction) => rpc<boolean>("chat.queue", { chatId, command }),
  goal: (chatId: string, command: GoalAction, permission: PermissionMode | null) => rpc<void>("chat.goal", { chatId, command, permission }),
  interrupt: (chatId: string) => rpc<void>("chat.interrupt", { chatId }),
  stopTask: (chatId: string, taskId: string) => rpc<void>("chat.stopTask", { chatId, taskId }),
  answer: (chatId: string, requestId: string, decision: Decision) => rpc<void>("chat.answer", { chatId, requestId, decision }),
  answerQuestion: (chatId: string, requestId: string, answers: Record<string, string[]>) => rpc<void>("chat.answerQuestion", { chatId, requestId, answers }),
  setModel: (chatId: string, model: string, effort: string, fastMode?: boolean) => rpc<ChatMeta>("chat.setModel", { chatId, model, effort, fastMode }),
  setEffort: (chatId: string, effort: string) => rpc<ChatMeta>("chat.setEffort", { chatId, effort }),
  setPermission: (chatId: string, mode: PermissionMode) => rpc<void>("chat.setPermission", { chatId, mode }),
  compact: (chatId: string, instructions: string, permission: PermissionMode | null) => rpc<void>("chat.compact", { chatId, instructions, permission }),
  clear: (chatId: string) => rpc<void>("chat.clear", { chatId }),
  readImage: (chatId: string, path: string) => rpc<string>("chat.readImage", { chatId, path }),
  image: (chatId: string, file: string) => rpc<string>("chat.image", { chatId, file }),
  rename: (chatId: string, title: string) => rpc<ChatMeta>("chat.rename", { chatId, title }),
  sleep: (chatId: string) => rpc<void>("chat.sleep", { chatId }),
  remove: (chatId: string) => rpc<void>("chat.delete", { chatId }),
  catalog: (kind: AgentKind, projectId: string | null) => rpc<Catalog>("provider.catalog", { kind, projectId }),
}

/** What this phone calls itself when it pairs: its model, from the WebView's user agent. */
export function phoneName(ua = navigator.userAgent): string {
  if (!/Android/.test(ua)) return "Phone"
  const m = /Android [\d.]+; ([^;)]+?)(?: Build\/[^;)]*)?[;)]/.exec(ua)
  const model = m?.[1]?.trim()
  return model && model !== "K" && !/^wv$/i.test(model) ? model : "Android phone"
}
