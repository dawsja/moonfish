import { beforeEach, describe, expect, it, vi } from "vitest"
import { ipc } from "@/lib/ipc"
import type { Limits, Location, Settings } from "@/lib/types"
import {
  LOCAL_USAGE,
  dedupeUsageByAccount,
  nextReset,
  resetsIn,
  shortLabel,
  shownAgents,
  usageLocationKey,
  usageLocations,
  useUsage,
} from "./usage"

vi.mock("@/lib/ipc", () => ({ ipc: { providerLimits: vi.fn() } }))

const w = (label: string, resetsAt: number | null = null) => ({ label, usedPercent: 10, resetsAt })

describe("usage labels", () => {
  it("keeps a model group's name on its windows (Antigravity)", () => {
    expect(shortLabel(w("Gemini · 5 hours"))).toBe("Gemini 5h")
    expect(shortLabel(w("Claude and GPT · this week"))).toBe("Claude and GPT wk")
  })

  it("shortens window names", () => {
    expect(shortLabel(w("Current session (5 hours)"))).toBe("5h")
    expect(shortLabel(w("This week, all models"))).toBe("wk")
    expect(shortLabel(w("This week, Fable"))).toBe("Fable")
    expect(shortLabel(w("Weekly window"))).toBe("wk")
    expect(shortLabel(w("This month"))).toBe("30d")
    expect(shortLabel(w("2 day window"))).toBe("2d")
  })
  it("says when the next reset is", () => {
    const now = 1_000_000_000_000
    expect(resetsIn(now + (2 * 1440 + 17 * 60) * 60_000, now)).toBe("in 2d 17h")
    expect(resetsIn(now + (4 * 60 + 47) * 60_000, now)).toBe("in 4h 47m")
    expect(resetsIn(now + 12 * 60_000, now)).toBe("in 12m")
    expect(resetsIn(null)).toBeNull()
    expect(nextReset({ windows: [w("a", 5), w("b", 3), w("c")], error: null, extra: [] })).toBe(3)
  })
})

describe("usage locations", () => {
  const ubuntu: Location = { kind: "wsl", distro: "Ubuntu" }
  const ssh: Location = { kind: "ssh", host: "build" }
  const limits = (used: number): Limits => ({ windows: [{ ...w("Weekly window"), usedPercent: used }], error: null, extra: [] })
  beforeEach(() => {
    vi.mocked(ipc.providerLimits).mockReset()
    useUsage.setState({ entries: {} })
  })

  it("queries WSL directly and keeps local and SSH usage separate", async () => {
    vi.mocked(ipc.providerLimits).mockImplementation(async (_agent, location) => limits(location?.kind === "wsl" ? 30 : location?.kind === "ssh" ? 60 : 10))
    for (const location of [LOCAL_USAGE, ubuntu, ssh]) await useUsage.getState().load(location)
    expect(ipc.providerLimits).toHaveBeenCalledWith("claude", ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledWith("codex", ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledWith("antigravity", ubuntu)
    const entries = useUsage.getState().entries
    expect(entries[usageLocationKey(ubuntu)].limits.claude?.windows[0].usedPercent).toBe(30)
    expect(entries[usageLocationKey(LOCAL_USAGE)].limits.claude?.windows[0].usedPercent).toBe(10)
    expect(entries[usageLocationKey(ssh)].limits.claude?.windows[0].usedPercent).toBe(60)
    await useUsage.getState().load(ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(9)
    await useUsage.getState().load(ubuntu, true)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(12)
  })

  it("loads multiple unique locations in parallel with loadAll and supports invalidate", async () => {
    vi.mocked(ipc.providerLimits).mockImplementation(async (_agent, location) => limits(location?.kind === "ssh" ? 55 : 15))
    await useUsage.getState().loadAll([LOCAL_USAGE, ssh, LOCAL_USAGE])
    expect(ipc.providerLimits).toHaveBeenCalledTimes(6)
    expect(useUsage.getState().entries.local.limits.claude?.windows[0].usedPercent).toBe(15)
    expect(useUsage.getState().entries[usageLocationKey(ssh)].limits.claude?.windows[0].usedPercent).toBe(55)
    useUsage.getState().invalidate(ssh, "claude")
    await useUsage.getState().loadAll([LOCAL_USAGE, ssh])
    expect(ipc.providerLimits).toHaveBeenCalledTimes(7)
    expect(ipc.providerLimits).toHaveBeenLastCalledWith("claude", ssh)
  })

  it("asks again for an agent that failed (signed out, no key), but reuses ones that answered", async () => {
    let signedIn = false
    vi.mocked(ipc.providerLimits).mockImplementation(async (agent) =>
      agent === "codex" && !signedIn ? { windows: [], error: "Codex sign-in required.", extra: [] } : limits(40),
    )
    await useUsage.getState().load(ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(3)
    signedIn = true
    await useUsage.getState().load(ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(4)
    expect(ipc.providerLimits).toHaveBeenLastCalledWith("codex", ubuntu)
    expect(useUsage.getState().entries[usageLocationKey(ubuntu)].limits.codex?.error).toBeNull()
    await useUsage.getState().load(ubuntu)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(4)
  })

  it("a thrown request counts as a failure and is asked again", async () => {
    vi.mocked(ipc.providerLimits).mockImplementation(async (agent) => {
      if (agent === "codex") throw new Error("Codex didn't start")
      return limits(5)
    })
    await useUsage.getState().load(LOCAL_USAGE)
    expect(useUsage.getState().entries.local.limits.codex?.error).toContain("Codex didn't start")
    await useUsage.getState().load(LOCAL_USAGE)
    expect(ipc.providerLimits).toHaveBeenCalledTimes(4)
    expect(ipc.providerLimits).toHaveBeenLastCalledWith("codex", LOCAL_USAGE)
  })

  it("does not let slow local failures overwrite WSL results after switching", async () => {
    let finish!: (result: Limits) => void
    const pending = new Promise<Limits>((resolve) => { finish = resolve })
    vi.mocked(ipc.providerLimits).mockImplementation((_agent, location) => location?.kind === "local" ? pending : Promise.resolve(limits(25)))
    const localLoad = useUsage.getState().load(LOCAL_USAGE)
    await useUsage.getState().load(ubuntu)
    finish({ windows: [], error: "Local credential rejected", extra: [] })
    await localLoad
    expect(useUsage.getState().entries[usageLocationKey(ubuntu)].limits.claude?.error).toBeNull()
    expect(useUsage.getState().entries[usageLocationKey(ubuntu)].limits.claude?.windows[0].usedPercent).toBe(25)
    expect(useUsage.getState().entries.local.limits.claude?.error).toBe("Local credential rejected")
  })
})

describe("usageLocations", () => {
  it("orders active location first, deduplicates, and skips stopped WSL distros", () => {
    const settings = {
      sshHosts: [{ host: "mac", port: 22, identityFile: "", label: "" }],
      projects: [
        { id: "1", name: "a", location: { kind: "ssh", host: "mac" } },
        { id: "2", name: "b", location: { kind: "ssh", host: "gpu" } },
        { id: "3", name: "c", location: { kind: "wsl", distro: "Ubuntu" } },
        { id: "4", name: "d", location: { kind: "wsl", distro: "Debian" } },
      ],
    } as unknown as Settings

    const locs = usageLocations(settings, { kind: "ssh", host: "gpu" }, { Ubuntu: true, Debian: false })
    expect(locs).toEqual([
      { kind: "ssh", host: "gpu" },
      { kind: "local" },
      { kind: "ssh", host: "mac" },
      { kind: "wsl", distro: "Ubuntu" },
    ])
  })
})

describe("dedupeUsageByAccount", () => {
  const ok = (used: number, email?: string | null, plan?: string | null): Limits => ({
    windows: [{ ...w("Weekly window"), usedPercent: used }],
    error: null,
    extra: [],
    accountEmail: email,
    accountPlan: plan,
  })
  const err = (message: string, email?: string | null): Limits => ({
    windows: [],
    error: message,
    extra: [],
    accountEmail: email,
  })
  const missing: Limits = { windows: [], error: null, extra: [], missing: true }

  it("collapses the same account across multiple locations (case-insensitive email)", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { claude: ok(42, "Alice@Example.com", "max") } },
        { location: "SSH · mac", limits: { claude: ok(45, "alice@example.com", "max") } },
      ],
      ["claude"],
    )
    expect(rows).toHaveLength(1)
    expect(rows[0].agent).toBe("claude")
    expect(rows[0].email).toBe("Alice@Example.com")
    expect(rows[0].plan).toBe("max")
    expect(rows[0].locations).toEqual(["This computer", "SSH · mac"])
    expect(rows[0].limits.windows[0].usedPercent).toBe(42)
  })

  it("keeps distinct accounts for the same agent as separate rows", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { codex: ok(20, "personal@example.com", "plus") } },
        { location: "SSH · work", limits: { codex: ok(75, "work@company.com", "pro") } },
      ],
      ["codex"],
    )
    expect(rows).toHaveLength(2)
    expect(rows[0]).toMatchObject({ agent: "codex", email: "personal@example.com", plan: "plus", locations: ["This computer"] })
    expect(rows[1]).toMatchObject({ agent: "codex", email: "work@company.com", plan: "pro", locations: ["SSH · work"] })
  })

  it("shows remote-only usage and suppresses local not-signed-in and missing errors", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { claude: err("Not signed in"), codex: missing } },
        { location: "SSH · mac", limits: { claude: ok(33, "dev@example.com", "pro"), codex: ok(18, "dev@example.com", "plus") } },
      ],
      ["claude", "codex"],
    )
    expect(rows).toHaveLength(2)
    expect(rows[0]).toMatchObject({ agent: "claude", email: "dev@example.com", locations: ["SSH · mac"] })
    expect(rows[0].limits.error).toBeNull()
    expect(rows[1]).toMatchObject({ agent: "codex", email: "dev@example.com", locations: ["SSH · mac"] })
    expect(rows[1].limits.error).toBeNull()
  })

  it("collapses errors into one row when no location has a healthy result", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { claude: err("Not signed in") } },
        { location: "SSH · mac", limits: { claude: err("Sign in on mac") } },
      ],
      ["claude"],
    )
    expect(rows).toHaveLength(1)
    expect(rows[0].limits.error).toBe("Not signed in")
    expect(rows[0].locations).toEqual(["This computer", "SSH · mac"])
  })

  it("omits agents that are missing everywhere", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { antigravity: missing } },
        { location: "SSH · mac", limits: { antigravity: missing } },
      ],
      ["antigravity"],
    )
    expect(rows).toEqual([])
  })

  it("prefers healthy limits with populated windows when collapsing same email", () => {
    const rows = dedupeUsageByAccount(
      [
        { location: "This computer", limits: { claude: { windows: [], error: null, extra: [], accountEmail: "a@b.com" } } },
        { location: "SSH · mac", limits: { claude: ok(64, "a@b.com", "max") } },
      ],
      ["claude"],
    )
    expect(rows).toHaveLength(1)
    expect(rows[0].limits.windows).toHaveLength(1)
    expect(rows[0].limits.windows[0].usedPercent).toBe(64)
    expect(rows[0].plan).toBe("max")
    expect(rows[0].locations).toEqual(["This computer", "SSH · mac"])
  })
})

describe("shownAgents", () => {
  const settings = (codexOn: boolean) =>
    ({ providers: { claude: { enabled: true }, codex: { enabled: codexOn }, antigravity: { enabled: true } } }) as unknown as Settings
  const none = { windows: [], error: null, extra: [] }

  it("leaves out agents not installed there, and ones turned off", () => {
    expect(shownAgents(settings(true), { claude: none, codex: { ...none, missing: true }, antigravity: { ...none, missing: true } })).toEqual(["claude"])
    expect(shownAgents(settings(false), {})).toEqual(["claude", "antigravity"])
    // Not loaded yet: shown (loading), until the app says they're missing.
    expect(shownAgents(settings(true), {})).toEqual(["claude", "codex", "antigravity"])
  })
})
