import { beforeEach, describe, expect, it, vi } from "vitest"

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  isTauri: () => false,
  Channel: class {
    onmessage: (m: unknown) => void = () => {}
  },
}))

import { permissionNote } from "./Alerts"
import { openAlert, useMobile } from "./store"
import type { AlertsState, Desktop } from "./rpc"

const desktop = (id: string): Desktop => ({ id, name: id, addresses: [], deviceId: "d", pairedAt: 0, lastAddress: null })

describe("the permission note", () => {
  const state = (o: Partial<AlertsState>): AlertsState => ({ enabled: false, done: false, permission: "prompt", ...o })

  it("says nothing before alerts are turned on", () => {
    expect(permissionNote(state({ permission: "prompt" }), false)).toBeNull()
    expect(permissionNote(state({ permission: "denied" }), false)).toBeNull()
    expect(permissionNote(state({ permission: "granted", enabled: true }), true)).toBeNull()
  })

  it("explains a refusal, and a block in Android's settings", () => {
    expect(permissionNote(state({ permission: "prompt" }), true)).toMatch(/choose Allow/)
    expect(permissionNote(state({ permission: "denied" }), true)).toMatch(/Android's settings/)
  })

  it("warns when alerts are on but Android blocks them", () => {
    expect(permissionNote(state({ permission: "denied", enabled: true }), false)).toMatch(/can't show/)
  })
})

describe("a tapped notification", () => {
  beforeEach(() => {
    const stored = new Map<string, string>()
    vi.stubGlobal("localStorage", { getItem: (k: string) => stored.get(k) ?? null, setItem: (k: string, v: string) => stored.set(k, v), removeItem: (k: string) => stored.delete(k) })
    vi.stubGlobal("location", { hash: "#/" })
    vi.stubGlobal("history", { state: null, length: 1 })
    invoke.mockReset()
    invoke.mockResolvedValue(undefined)
    useMobile.setState({ desktops: [desktop("home"), desktop("work")], desktopId: "home", link: "connected", chatId: null })
  })

  it("opens its chat on the desktop already connected", async () => {
    await openAlert({ chatId: "c1", desktopId: "home" })
    expect(location.hash).toBe("#/chat/c1")
    expect(invoke.mock.calls.find(([cmd]) => cmd === "connect")).toBeUndefined()
  })

  it("connects to its desktop first", async () => {
    await openAlert({ chatId: "c2", desktopId: "work" })
    expect(invoke).toHaveBeenCalledWith("connect", expect.objectContaining({ id: "work" }))
    expect(useMobile.getState().desktopId).toBe("work")
    expect(location.hash).toBe("#/chat/c2")
  })

  it("leaves the screen alone for a desktop no longer paired", async () => {
    await openAlert({ chatId: "c3", desktopId: "gone" })
    expect(location.hash).toBe("#/")
    expect(useMobile.getState().desktopId).toBe("home")
  })
})
