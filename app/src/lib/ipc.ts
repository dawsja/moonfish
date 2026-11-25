// The commands the Rust side exposes (app/src-tauri/src/lib.rs).

import type { GoalAction } from "./goals"
import type { ChatEvent, ChatMeta, Decision, ModelOption, QueueAction, SearchHit, SendMode, SlashCommand, Snapshot, SubagentSnapshot, Waiting } from "./chat"
import type { Account, AgentKind, AppInfo, Distro, HostCheck, Limits, Location, LoginEvent, LoginMethod, LoginStart, PermissionMode, Probe, Project, Settings } from "./types"

/** An agent's models and slash commands, for a chat that hasn't started. */
export type Catalog = { models: ModelOption[]; commands: SlashCommand[] }

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core")
  return invoke<T>(cmd, args)
}

export const ipc = {
  appInfo: () => call<AppInfo>("app_info"),
  settingsGet: () => call<Settings>("settings_get"),
  settingsSet: (settings: Settings) => call<Settings>("settings_set", { settings }),
  projectAdd: (location: Location, path: string) => call<[Settings, Project]>("project_add", { location, path }),
  projectRemove: (id: string) => call<Settings>("project_remove", { id }),
  sshConfigHosts: () => call<string[]>("ssh_config_hosts"),
  sshConfigRead: () => call<string | null>("ssh_config_read"),
  sshConfigSave: (original: string | null, text: string) => call<string[]>("ssh_config_save", { original, text }),
  hostCheck: (location: Location) => call<HostCheck>("location_check", { location }),
  hostListDir: (location: Location, path: string) => call<{ path: string; dirs: string[] }>("location_list_dir", { location, path }),
  wslDistros: () => call<Distro[]>("wsl_distros"),
  wslStart: (distro: string) => call<void>("wsl_start", { distro }),
  wslPath: (path: string) => call<{ distro: string; path: string } | null>("wsl_path", { path }),
  providerProbe: (kind: AgentKind, binary: string) => call<Probe>("provider_probe", { kind, binary }),
  providerAccount: (kind: AgentKind, location: Location | null = null) => call<Account>("provider_account", { kind, location }),
  providerLimits: (kind: AgentKind, location: Location | null = null) => call<Limits>("provider_limits", { kind, location }),
  providerLoginStart: (agent: AgentKind, location: Location | null = null, method: LoginMethod | null = null) =>
    call<LoginStart>("provider_login_start", { agent, location, method }),
  providerLoginSubmit: (sessionId: string, input: string) => call<void>("provider_login_submit", { sessionId, input }),
  providerLoginCancel: (sessionId: string) => call<void>("provider_login_cancel", { sessionId }),

  chatList: () => call<ChatMeta[]>("chat_list"),
  /** What each chat waits on the user for, open in this window or not. */
  chatWaiting: () => call<Record<string, Waiting>>("chat_waiting"),
  /** Chats with background work running. */
  chatBackground: () => call<string[]>("chat_background"),
  chatBusy: () => call<string[]>("chat_busy"),
  chatsStopAll: () => call<void>("chats_stop_all"),
  chatStopTask: (chatId: string, taskId: string) => call<void>("chat_stop_task", { chatId, taskId }),
  chatSubagentSnapshot: (chatId: string, taskId: string) => call<SubagentSnapshot>("chat_subagent_snapshot", { chatId, taskId }),
  chatSubagentSend: (chatId: string, taskId: string, prompt: { id: string; text: string; images: { mime: string; data: string }[] }) =>
    call<void>("chat_subagent_send", { chatId, taskId, prompt }),
  chatSubagentInterrupt: (chatId: string, taskId: string) => call<void>("chat_subagent_interrupt", { chatId, taskId }),
  /** Chats whose text has every word of `query`, newest first. */
  chatSearch: (query: string) => call<SearchHit[]>("chat_search", { query }),
  /** The chat as Markdown, for an issue or a PR description. */
  chatMarkdown: (chatId: string) => call<string>("chat_markdown", { chatId }),
  /** The taskbar/dock badge and window title: how many chats wait (`icon`: Windows' overlay, square RGBA). */
  setBadge: (count: number, icon: number[] | null) => call<void>("app_set_badge", { count, icon }),
  chatCreate: (projectId: string, agent: AgentKind | null, model: string | null, effort: string | null, cwd: string | null, fastMode?: boolean | null) =>
    call<ChatMeta>("chat_create", { projectId, agent, model, effort, cwd, fastMode }),
  providerCatalog: (kind: AgentKind, projectId: string | null) => call<Catalog>("provider_catalog", { kind, projectId }),
  /** The chat as it is now, mid-turn included. */
  chatSnapshot: (chatId: string) => call<Snapshot>("chat_snapshot", { chatId }),
  chatSend: (chatId: string, prompt: { id: string; text: string; images: { mime: string; data: string }[] }, permission: PermissionMode | null, mode?: SendMode) =>
    call<void>("chat_send", { chatId, prompt, permission, mode }),
  chatQueue: (chatId: string, command: QueueAction) => call<boolean>("chat_queue", { chatId, command }),
  chatCompact: (chatId: string, instructions: string, permission: PermissionMode | null) => call<void>("chat_compact", { chatId, instructions, permission }),
  chatClear: (chatId: string) => call<void>("chat_clear", { chatId }),
  chatGoal: (chatId: string, command: GoalAction, permission: PermissionMode | null) => call<void>("chat_goal", { chatId, command, permission }),
  chatInterrupt: (chatId: string) => call<void>("chat_interrupt", { chatId }),
  chatAnswer: (chatId: string, requestId: string, decision: Decision) => call<void>("chat_answer", { chatId, requestId, decision }),
  chatSetPermission: (chatId: string, mode: PermissionMode) => call<void>("chat_set_permission", { chatId, mode }),
  chatAnswerQuestion: (chatId: string, requestId: string, answers: Record<string, string[]>) => call<void>("chat_answer_question", { chatId, requestId, answers }),
  chatSetModel: (chatId: string, model: string, effort: string, fastMode?: boolean) => call<ChatMeta>("chat_set_model", { chatId, model, effort, fastMode }),
  chatSetEffort: (chatId: string, effort: string) => call<ChatMeta>("chat_set_effort", { chatId, effort }),
  /** An image saved with a chat, as a `data:` URL. */
  chatImage: (chatId: string, file: string) => call<string>("chat_image", { chatId, file }),
  chatSleep: (chatId: string) => call<void>("chat_sleep", { chatId }),
  chatDelete: (chatId: string) => call<void>("chat_delete", { chatId }),
  chatRename: (chatId: string, title: string) => call<ChatMeta>("chat_rename", { chatId, title }),
  chatPin: (chatId: string, pinned: boolean) => call<ChatMeta>("chat_pin", { chatId, pinned }),
}

/** Calls `fn` for every chat event the app emits (`seq`: its number in the chat, as in a
 *  snapshot); returns the unsubscribe. */
export async function onChatEvent(fn: (chatId: string, event: ChatEvent, seq: number) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen<{ chatId: string; event: ChatEvent; seq: number }>("chat-event", (e) => fn(e.payload.chatId, e.payload.event, e.payload.seq))
}

export async function onSubagentEvent(fn: (chatId: string, taskId: string, event: ChatEvent, seq: number) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen<{ chatId: string; taskId: string; event: ChatEvent; seq: number }>("subagent-event", (e) =>
    fn(e.payload.chatId, e.payload.taskId, e.payload.event, e.payload.seq),
  )
}

/** Calls `fn` when chats were added, renamed or removed from a phone, or a turn started or ended. */
export async function onChatsChanged(fn: () => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen("chats-changed", () => fn())
}

/** Calls `fn` when a notification about a chat was clicked (`action`: `tap`, or `answer` for
 *  its Answer button); the window is already in front. */
export async function onNotificationAction(fn: (chatId: string, action: string) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen<{ chatId: string; action: string }>("notification-action", (e) => fn(e.payload.chatId, e.payload.action))
}

/** Calls `fn` for provider login progress, completion, or error events; returns the unsubscribe. */
export async function onLoginEvent(fn: (event: LoginEvent) => void): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event")
  return listen<LoginEvent>("login-event", (e) => fn(e.payload))
}

/** Picks a folder on this computer; null when cancelled. */
export async function pickFolder(title: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog")
  const r = await open({ directory: true, multiple: false, title })
  return typeof r === "string" ? r : null
}
