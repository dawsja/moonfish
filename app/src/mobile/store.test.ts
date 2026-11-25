import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock("@tauri-apps/api/core", () => ({ invoke }))

import { useMobile } from "./store"
import { api, type AgentInfo } from "./rpc"
import type { ChatMeta } from "@/lib/chat"

const meta = (effort: string | null) => ({ id: "chat-new", projectId: "p1", agent: "claude", model: "opus", effort, title: "New chat" })

beforeEach(() => {
  useMobile.setState({ metas: [], chatId: null, transcripts: {}, desktopId: null, link: "idle" })
  invoke.mockReset()
  invoke.mockImplementation(async (_cmd: string, args: { method: string; params: Record<string, unknown> }) => {
    switch (args.method) {
      case "chat.create": return meta(null)
      case "chat.setEffort": return meta(args.params.effort as string)
      case "chat.open": return { events: [], busy: false, live: false }
      default: return undefined
    }
  })
})
afterEach(() => vi.restoreAllMocks())

describe("a new chat from the phone", () => {
  const agent = { kind: "claude", name: "Claude Code" } as AgentInfo

  it("starts with the effort picked before the first message", async () => {
    const id = await useMobile.getState().send(null, "hi", [], { projectId: "p1", agent, model: "opus", effort: "high" })
    expect(id).toBe("chat-new")
    const methods = invoke.mock.calls.map(([, a]) => a.method)
    expect(methods).toEqual(["chat.create", "chat.setEffort", "chat.open", "chat.send"])
    expect(invoke.mock.calls[1][1].params).toEqual({ chatId: "chat-new", effort: "high" })
    expect(useMobile.getState().metas.find((m) => m.id === "chat-new")?.effort).toBe("high")
  })

  it("leaves the agent's default effort alone when none was picked", async () => {
    await useMobile.getState().send(null, "hi", [], { projectId: "p1", agent, model: "opus" })
    expect(invoke.mock.calls.map(([, a]) => a.method)).not.toContain("chat.setEffort")
  })

  it("sends its first message with the permission mode picked before it, and keeps it", async () => {
    const id = await useMobile.getState().send(null, "hi", [], { projectId: "p1", agent, permission: "plan" })
    const send = invoke.mock.calls.find(([, a]) => a.method === "chat.send")!
    expect(send[1].params.permission).toBe("plan")
    expect(useMobile.getState().metas.find((m) => m.id === id)?.permission).toBe("plan")
  })

  it("leaves the agent's default mode to the desktop when none was picked", async () => {
    await useMobile.getState().send(null, "hi", [], { projectId: "p1", agent })
    expect(invoke.mock.calls.find(([, a]) => a.method === "chat.send")![1].params.permission).toBeNull()
  })
})

describe("chat state shared with the desktop", () => {
  const saved = {
    ...meta("high"), permission: "ask", queued: [{ id: "reply-from-desktop", text: "Run the full suite", images: ["queue-test.png"] }],
    goal: { objective: "Fix the watcher", status: "paused" }, fastMode: true,
  } as ChatMeta

  it("loads the host's queue and goal and never dispatches a reply on open", async () => {
    useMobile.setState({ link: "connected" })
    vi.spyOn(api, "chats").mockResolvedValue({ metas: [saved], running: [], busy: [] })
    vi.spyOn(api, "projects").mockResolvedValue([])
    vi.spyOn(api, "agents").mockResolvedValue({ agents: [], defaultAgent: "claude" })
    const store = useMobile.getState()
    await store.refresh()
    await store.open(saved.id)
    expect(useMobile.getState().metas).toEqual([saved])
    expect(invoke.mock.calls.map(([, a]) => a.method)).not.toContain("chat.send")
    expect(invoke.mock.calls.map(([, a]) => a.method)).not.toContain("chat.queue")
  })

  it("sends queue changes to the host and keeps a reply when a command fails", async () => {
    useMobile.setState({ metas: [saved] })
    invoke.mockImplementation(async (_cmd, { method }) => {
      if (method === "chat.queue") throw new Error("disconnected")
    })
    const store = useMobile.getState()
    await store.send(saved.id, "Reply from mobile", [], { mode: "queue" })
    expect(invoke.mock.calls[0][1]).toMatchObject({
      method: "chat.enqueue", params: { chatId: saved.id, permission: null, prompt: { text: "Reply from mobile" } },
    })
    const id = saved.queued![0].id
    expect(await store.queueAction(saved.id, { action: "send", id })).toBe(false)
    expect(invoke.mock.calls[1][1]).toMatchObject({ method: "chat.queue", params: { command: { action: "send", id } } })
    expect(useMobile.getState().metas[0].queued).toEqual(saved.queued)
  })

  it("keeps the latest queue, goal and settings when list replies arrive out of order", async () => {
    useMobile.setState({ link: "connected" })
    const listReplies: ((data: Awaited<ReturnType<typeof api.chats>>) => void)[] = []
    vi.spyOn(api, "chats").mockImplementation(() => new Promise((resolve) => listReplies.push(resolve)))
    vi.spyOn(api, "projects").mockResolvedValue([])
    vi.spyOn(api, "agents").mockResolvedValue({ agents: [], defaultAgent: "claude" })
    const store = useMobile.getState()
    const before = store.refresh()
    const after = store.refresh()
    await vi.waitFor(() => expect(listReplies).toHaveLength(2))
    const latest: ChatMeta = { ...saved, queued: [], goal: null, model: "sonnet", permission: "plan", fastMode: false }
    listReplies[1]({ metas: [latest], running: [], busy: [] })
    await after
    listReplies[0]({ metas: [saved], running: [], busy: [] })
    await before
    expect(useMobile.getState().metas).toEqual([latest])
  })
})

describe("the home screen's cards", () => {
  const approval = { requestId: "q1", itemId: "k1", kind: "command", title: "Run the tests", detail: "bun test", diff: null, canRemember: true }

  it("reads what a waiting chat asks, then closes it again", async () => {
    useMobile.setState({ link: "connected", desktopId: "d1", requests: {} })
    invoke.mockImplementation(async (_cmd: string, { method }: { method: string }) =>
      method === "chat.open" ? { events: [{ type: "approval", request: approval }], seq: 1, busy: true, live: true } : undefined,
    )
    await useMobile.getState().peek("c1")
    expect(useMobile.getState().requests.c1?.approval?.requestId).toBe("q1")
    await vi.waitFor(() => expect(invoke.mock.calls.map(([, a]) => a.method)).toEqual(["chat.open", "chat.close"]))
    // Nothing of it is kept as an open chat.
    expect(useMobile.getState().transcripts.c1).toBeUndefined()
  })

  it("leaves a peeked chat subscribed when it was opened meanwhile", async () => {
    useMobile.setState({ link: "connected", desktopId: "d1", requests: {} })
    let reply: ((v: unknown) => void) | null = null
    invoke.mockImplementation((_cmd: string, { method }: { method: string }) => (method === "chat.open" ? new Promise((r) => (reply = r)) : Promise.resolve()))
    const peeked = useMobile.getState().peek("c1")
    await vi.waitFor(() => expect(reply).not.toBeNull())
    useMobile.setState({ chatId: "c1" })
    reply!({ events: [], seq: 0, busy: false, live: false })
    await peeked
    await new Promise((r) => setTimeout(r, 20))
    expect(invoke.mock.calls.map(([, a]) => a.method)).not.toContain("chat.close")
  })
})

describe("provider catalogs", () => {
  const antigravity = { kind: "antigravity", name: "Antigravity" } as AgentInfo

  it("surfaces catalog errors instead of caching empty results", async () => {
    useMobile.setState({ catalogs: {} })
    invoke.mockRejectedValueOnce("Antigravity isn't signed in for SSH sessions on mac.")
    const failed = await useMobile.getState().catalog(antigravity, "p1")
    expect(failed).toEqual({ catalog: null, error: "Antigravity isn't signed in for SSH sessions on mac." })
    expect(useMobile.getState().catalogs).toEqual({})

    invoke.mockResolvedValueOnce({ models: [], commands: [] })
    const empty = await useMobile.getState().catalog(antigravity, "p1")
    expect(empty).toEqual({ catalog: { models: [], commands: [] }, error: null })
    expect(useMobile.getState().catalogs).toEqual({})

    const model = { id: "gemini-4-neon", name: "Gemini 4 Neon", description: "", efforts: ["low", "high"], defaultEffort: null }
    invoke.mockResolvedValueOnce({ models: [model], commands: [] })
    const loaded = await useMobile.getState().catalog(antigravity, "p1")
    expect(loaded).toEqual({ catalog: { models: [model], commands: [] }, error: null })
    expect(useMobile.getState().catalogs["antigravity:p1"]?.models).toEqual([model])
  })
})
