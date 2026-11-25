// App-wide state: settings (saved through Rust), what's on screen, and the layout.

import { create } from "zustand"
import { toast } from "sonner"
import { ipc } from "@/lib/ipc"
import { BASE_UI_SIZE, type AppInfo, type Appearance, type Location, type Settings } from "@/lib/types"
import type { Viewer } from "./git"

export type SettingsSection = "sessions" | "providers" | "appearance" | "usage" | "ssh" | "projects" | "phone"
export type RightTab = "files" | "scm" | "history" | "pr"

export type ActiveTarget = Partial<{
  projectId: string | null
  chatId: string | null
  draftCwd: string | null
  viewer: Viewer | null
}>

type Layout = {
  sidebarWidth: number
  sidebarOpen: boolean
  rightWidth: number
  rightOpen: boolean
  controlCollapsed: boolean
  rightTab: RightTab
}

type AppStore = {
  info: AppInfo | null
  settings: Settings | null
  error: string | null
  view: "chat" | "settings"
  section: SettingsSection
  /** A control the settings page focuses once it's shown (the id it was opened for). */
  settingsFocus: string | null
  projectId: string | null
  chatId: string | null
  /** The folder a new (unsent) chat will work in: a worktree, or null for the project. */
  draftCwd: string | null
  /** Something opened from the side panel, shown in the main area instead of the chat. */
  viewer: Viewer | null
  layout: Layout

  load(): Promise<void>
  /** Applies `edit` to a copy of the settings and saves it. */
  update(edit: (s: Settings) => void): Promise<void>
  /** Adds a project and opens it; false (after saying why) when it couldn't be added. */
  addProject(location: Location, path: string): Promise<boolean>
  removeProject(id: string): Promise<void>
  /** Shows Settings, at `section`; `focus` names a control there to focus. */
  openSettings(section?: SettingsSection, focus?: string): void
  closeSettings(): void
  /** Changes what the main area shows. Switching projects resets chat, worktree, and viewer unless specified. */
  setActive(target: ActiveTarget): void
  setLayout(patch: Partial<Layout>): void
}

const LAYOUT_KEY = "moonfish.layout"
export const defaultLayout: Readonly<Layout> = {
  sidebarWidth: 272,
  sidebarOpen: true,
  rightWidth: 340,
  rightOpen: true,
  controlCollapsed: false,
  rightTab: "scm",
}

function loadLayout(): Layout {
  try {
    const raw = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "{}")
    if (!raw || typeof raw !== "object") return defaultLayout
    const savedSidebarWidth = typeof raw.sidebarWidth === "number" ? raw.sidebarWidth : defaultLayout.sidebarWidth
    const sidebarOpen = typeof raw.sidebarOpen === "boolean" ? raw.sidebarOpen : savedSidebarWidth > 0
    return {
      sidebarWidth: savedSidebarWidth > 0 ? savedSidebarWidth : defaultLayout.sidebarWidth,
      sidebarOpen,
      rightWidth: typeof raw.rightWidth === "number" ? raw.rightWidth : defaultLayout.rightWidth,
      rightOpen: typeof raw.rightOpen === "boolean" ? raw.rightOpen : defaultLayout.rightOpen,
      controlCollapsed: typeof raw.controlCollapsed === "boolean" ? raw.controlCollapsed : defaultLayout.controlCollapsed,
      rightTab: raw.rightTab === "files" || raw.rightTab === "scm" || raw.rightTab === "history" || raw.rightTab === "pr" ? raw.rightTab : defaultLayout.rightTab,
    }
  } catch {
    return defaultLayout
  }
}

/** The settings as last saved, to go back to when a save fails. */
let lastSaved: Settings | null = null
let saveSeq = 0
let saving: Promise<void> = Promise.resolve()

function commit(target: ActiveTarget, more: Partial<AppStore> = {}) {
  const cur = useApp.getState()
  const projectId = target.projectId !== undefined ? target.projectId : cur.projectId
  const switched = target.projectId !== undefined && target.projectId !== cur.projectId
  const next = {
    projectId,
    chatId: target.chatId !== undefined ? target.chatId : switched ? null : cur.chatId,
    draftCwd: target.draftCwd !== undefined ? target.draftCwd : switched ? null : cur.draftCwd,
    viewer: target.viewer !== undefined ? target.viewer : switched ? null : cur.viewer,
  }
  useApp.setState({ ...next, ...more })
  try {
    if (projectId) localStorage.setItem("moonfish.project", projectId)
  } catch {
    /* ignore */
  }
}

export const useApp = create<AppStore>((set, get) => ({
  info: null,
  settings: null,
  error: null,
  view: "chat",
  section: "providers",
  settingsFocus: null,
  projectId: null,
  chatId: null,
  draftCwd: null,
  viewer: null,
  layout: loadLayout(),

  async load() {
    try {
      const [info, settings] = await Promise.all([ipc.appInfo(), ipc.settingsGet()])
      const last = localStorage.getItem("moonfish.project")
      const projectId = settings.projects.find((p) => p.id === last)?.id ?? settings.projects[0]?.id ?? null
      lastSaved = settings
      const known = (id: string | null) => settings.projects.some((p) => p.id === id)
      set((s) => ({
        info,
        settings,
        error: null,
        ...(known(s.projectId) ? {} : { projectId, chatId: null, draftCwd: null, viewer: null }),
      }))
      import("@/lib/updates").then((u) => u.updater.start(info.bundle))
      applyAppearance(settings.appearance)
      if (info.settingsProblem) toast.error("Settings were reset", { description: info.settingsProblem, duration: Infinity })
    } catch (e) {
      set({ error: String(e) })
    }
  },

  async update(edit) {
    const cur = get().settings
    if (!cur) return
    const next = structuredClone(cur)
    edit(next)
    set({ settings: next })
    if (JSON.stringify(next.appearance) !== JSON.stringify(cur.appearance)) applyAppearance(next.appearance)
    // Saves go one at a time, each with the settings as they are by then, and only the newest
    // answer is applied: quick edits can't land out of order or undo each other.
    const seq = ++saveSeq
    saving = saving.then(async () => {
      if (seq !== saveSeq) return
      const want = get().settings
      if (!want) return
      try {
        const saved = await ipc.settingsSet(want)
        lastSaved = saved
        if (seq === saveSeq) set({ settings: saved })
      } catch (e) {
        if (seq !== saveSeq) return
        const before = get().settings
        set({ settings: lastSaved })
        if (lastSaved && JSON.stringify(before?.appearance) !== JSON.stringify(lastSaved.appearance)) applyAppearance(lastSaved.appearance)
        toast.error("Couldn't save settings", { description: String(e) })
      }
    })
    await saving
  },

  async addProject(location, path) {
    try {
      const [settings, project] = await ipc.projectAdd(location, path)
      lastSaved = settings
      commit({ projectId: project.id, chatId: null, draftCwd: null, viewer: null }, { settings, view: "chat", error: null })
      return true
    } catch (e) {
      toast.error("Couldn't add the project", { description: String(e) })
      return false
    }
  },

  async removeProject(id) {
    const project = get().settings?.projects.find((p) => p.id === id)
    if (!project) return
    const { useChats } = await import("./chats")
    const chats = useChats.getState().metas.filter((m) => m.projectId === id)
    const { confirmAction } = await import("@/components/Prompt")
    const ok = await confirmAction({
      title: `Remove ${project.name} from Moonfish?`,
      description: `Moonfish keeps the folder on disk.${chats.length ? ` Moonfish stops running agents and restores the saved chats if you add the folder again.` : ""}`,
      confirm: "Remove",
      danger: true,
    })
    if (!ok) return
    try {
      const settings = await ipc.projectRemove(id)
      lastSaved = settings
      const first = settings.projects[0]?.id ?? null
      commit(get().projectId === id ? { projectId: first, chatId: null, draftCwd: null, viewer: null } : {}, { settings })
    } catch (e) {
      toast.error("Couldn't remove the project", { description: String(e) })
    }
  },

  openSettings(section, focus) {
    set({ view: "settings", section: section ?? get().section, settingsFocus: focus ?? null })
  },
  closeSettings() {
    set({ view: "chat" })
  },
  setActive(target) {
    commit(target)
  },
  setLayout(patch) {
    const layout = { ...get().layout, ...patch }
    set({ layout })
    try {
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout))
    } catch {
      /* ignore */
    }
  },
}))

// ---- Appearance ------------------------------------------------------------------------------

const media = typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null
let current: Appearance | null = null
media?.addEventListener("change", () => current && applyAppearance(current))

/** Puts the theme, sizes and fonts on <html>. */
export function applyAppearance(a: Appearance) {
  current = a
  const root = document.documentElement
  const dark = a.theme === "dark" || (a.theme === "system" && (media?.matches ?? true))
  root.classList.toggle("dark", dark)
  root.style.setProperty("--moonfish-chat-size", `${a.chatFontSize}px`)
  root.style.setProperty("--moonfish-sans", `"${a.sansFont}"`)
  root.style.setProperty("--moonfish-mono", `"${a.monoFont}"`)
  try {
    localStorage.setItem("moonfish.theme", a.theme)
  } catch {
    /* ignore */
  }
  import("@tauri-apps/api/window").then(({ getCurrentWindow }) =>
    getCurrentWindow()
      .setTheme(a.theme === "system" ? null : a.theme)
      .catch(() => {}),
  )
  // Interface size zooms the whole window (text, icons, spacing), like the browser's zoom.
  import("@tauri-apps/api/webview").then(({ getCurrentWebview }) =>
    getCurrentWebview()
      .setZoom(a.uiFontSize / BASE_UI_SIZE)
      .catch(() => {}),
  )
}

/** The active project. */
export function useProject() {
  return useApp((s) => s.settings?.projects.find((p) => p.id === s.projectId) ?? null)
}
