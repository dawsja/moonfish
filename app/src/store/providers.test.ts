import { beforeEach, describe, expect, it, vi } from "vitest"
import { availableAgents, mobileAvailableAgents, pickNewChatAgent } from "@/lib/agents"
import { ipc } from "@/lib/ipc"
import type { AgentKind, HostCheck, Location, Probe, Provider, Settings, Tool } from "@/lib/types"
import { providerLocationKey, toolToProbe, useProviders } from "./providers"

vi.mock("@/lib/ipc", () => ({
  ipc: {
    providerProbe: vi.fn(),
    hostCheck: vi.fn(),
  },
}))

const provider = (enabled = true, binary = ""): Provider => ({
  enabled,
  binary,
  defaultModel: "",
  permissionMode: "ask",
  env: [],
})

const makeSettings = (overrides: Partial<Record<AgentKind, Partial<Provider>>> = {}, defaultAgent: AgentKind = "claude"): Settings =>
  ({
    providers: {
      claude: { ...provider(), ...overrides.claude },
      codex: { ...provider(), ...overrides.codex },
      antigravity: { ...provider(), ...overrides.antigravity },
      defaultAgent,
    },
  }) as unknown as Settings

const probe = (found: boolean, path: string | null = found ? "/usr/local/bin/cli" : null): Probe => ({
  found,
  path,
  version: found ? "1.2.3" : null,
  error: null,
  outdated: null,
})

const tool = (path: string | null, version: string | null = path ? "1.0.0" : null, windows: string | null = null): Tool => ({
  path,
  version,
  windows,
})

const hostCheck = (installed: Partial<Record<AgentKind, boolean>>): HostCheck => ({
  ok: true,
  error: null,
  home: "/home/dev",
  os: "Linux",
  git: tool("/usr/bin/git"),
  claude: tool(installed.claude ? "/usr/local/bin/claude" : null),
  codex: tool(installed.codex ? "/usr/local/bin/codex" : null),
  antigravity: tool(installed.antigravity ? "/usr/local/bin/agy" : null),
})

function installMemoryStorage() {
  const map = new Map<string, string>()
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => void map.clear(),
  })
  return map
}

describe("availableAgents", () => {
  it("excludes uninstalled providers that have not been explicitly added", () => {
    const settings = makeSettings()
    const probes = {
      claude: probe(true),
      codex: probe(false),
      antigravity: probe(false),
    }
    expect(availableAgents(settings, { probes })).toEqual(["claude"])
  })

  it("includes an uninstalled provider when the user explicitly added it (shown)", () => {
    const settings = makeSettings()
    const probes = {
      claude: probe(true),
      codex: probe(false),
      antigravity: probe(false),
    }
    expect(availableAgents(settings, { choices: { codex: "shown" }, probes })).toEqual(["claude", "codex"])
  })

  it("never shows a disabled provider even when installed or marked shown", () => {
    const settings = makeSettings({ codex: { enabled: false } })
    const probes = {
      claude: probe(true),
      codex: probe(true),
      antigravity: probe(false),
    }
    expect(availableAgents(settings, { choices: { codex: "shown" }, probes })).toEqual(["claude"])
  })

  it("excludes an installed provider when explicitly hidden", () => {
    const settings = makeSettings()
    const probes = {
      claude: probe(true),
      codex: probe(true),
      antigravity: probe(false),
    }
    expect(availableAgents(settings, { choices: { codex: "hidden" }, probes })).toEqual(["claude"])
  })

  it("avoids emptying the picker while probes are still loading", () => {
    const settings = makeSettings()
    expect(availableAgents(settings, { choices: { antigravity: "hidden" }, probes: {} })).toEqual(["claude", "codex"])
    // Partial probe resolution: codex resolved as missing while claude is still probing.
    expect(availableAgents(settings, { probes: { codex: probe(false) } })).toEqual(["claude", "antigravity"])
  })

  it("falls back to enabled providers when nothing is installed so the picker is never empty", () => {
    const settings = makeSettings({ antigravity: { enabled: false } })
    const probes = {
      claude: probe(false),
      codex: probe(false),
      antigravity: probe(false),
    }
    expect(availableAgents(settings, { probes })).toEqual(["claude", "codex"])
    // Respects hidden in fallback when another enabled provider remains.
    expect(availableAgents(settings, { choices: { codex: "hidden" }, probes })).toEqual(["claude"])
    // Falls back to all enabled providers if every enabled provider was hidden.
    expect(availableAgents(settings, { choices: { claude: "hidden", codex: "hidden" }, probes })).toEqual(["claude", "codex"])
  })

  it("always keeps the currently selected agent visible in canonical order", () => {
    const settings = makeSettings({ codex: { enabled: false } })
    const probes = {
      claude: probe(true),
      codex: probe(false),
      antigravity: probe(true),
    }
    expect(availableAgents(settings, { probes, selected: "codex" })).toEqual(["claude", "codex", "antigravity"])
  })
})

describe("pickNewChatAgent", () => {
  it("picks the first available agent when defaultAgent is unavailable", () => {
    expect(pickNewChatAgent("codex", ["claude"])).toBe("claude")
    expect(pickNewChatAgent("codex", ["claude", "codex"])).toBe("codex")
    expect(pickNewChatAgent("claude", ["claude", "codex"], "codex")).toBe("codex")
    // If a previously picked agent becomes unavailable (e.g. hidden in Settings), falls back cleanly.
    expect(pickNewChatAgent("claude", ["claude"], "codex")).toBe("claude")
  })
})

describe("mobileAvailableAgents", () => {
  const local: Location = { kind: "local" }
  const ssh: Location = { kind: "ssh", host: "devbox" }

  it("keeps all agents when desktop omits optional availability fields", () => {
    const agents = [
      { kind: "claude" as const, name: "Claude Code" },
      { kind: "codex" as const, name: "Codex" },
    ]
    expect(mobileAvailableAgents(agents, local)).toEqual(agents)
  })

  it("respects enabled, available, and local installed flags with non-empty fallback", () => {
    const agents = [
      { kind: "claude" as const, enabled: true, installed: true },
      { kind: "codex" as const, enabled: true, installed: false },
      { kind: "antigravity" as const, enabled: false, installed: true },
    ]
    expect(mobileAvailableAgents(agents, local).map((a) => a.kind)).toEqual(["claude"])
    // On an SSH project, local installed=false does not hide an enabled agent unless available=false.
    expect(mobileAvailableAgents(agents, ssh).map((a) => a.kind)).toEqual(["claude", "codex"])
    expect(
      mobileAvailableAgents(
        [
          { kind: "claude" as const, enabled: true, available: true },
          { kind: "codex" as const, enabled: true, available: false },
        ],
        ssh,
      ).map((a) => a.kind),
    ).toEqual(["claude"])
    // Fallback when all enabled agents report installed=false locally.
    expect(
      mobileAvailableAgents(
        [
          { kind: "claude" as const, enabled: true, installed: false },
          { kind: "codex" as const, enabled: true, installed: false },
        ],
        local,
      ).map((a) => a.kind),
    ).toEqual(["claude", "codex"])
  })
})

describe("useProviders store", () => {
  const local: Location = { kind: "local" }
  const ssh: Location = { kind: "ssh", host: "gpu-box" }

  beforeEach(() => {
    installMemoryStorage()
    vi.mocked(ipc.providerProbe).mockReset()
    vi.mocked(ipc.hostCheck).mockReset()
    useProviders.getState().invalidate()
    useProviders.setState({ choices: {}, byLocation: {} })
  })

  it("persists and reactively updates provider list choices", () => {
    useProviders.getState().setChoice("codex", "shown")
    expect(useProviders.getState().choices).toEqual({ codex: "shown" })
    useProviders.setState({ choices: {} })
    useProviders.getState().reloadChoices()
    expect(useProviders.getState().choices).toEqual({ codex: "shown" })

    useProviders.getState().setChoice("codex", "hidden")
    expect(useProviders.getState().choices).toEqual({ codex: "hidden" })
  })

  it("probes local and remote SSH locations separately and reflects remote installed CLIs", async () => {
    const settings = makeSettings()
    vi.mocked(ipc.providerProbe).mockImplementation(async (kind) => probe(kind === "claude"))
    vi.mocked(ipc.hostCheck).mockResolvedValue(hostCheck({ claude: false, codex: true, antigravity: false }))

    await useProviders.getState().probeLocation(local, settings.providers)
    await useProviders.getState().probeLocation(ssh, settings.providers)

    const localProbes = useProviders.getState().byLocation[providerLocationKey(local)].probes
    const sshProbes = useProviders.getState().byLocation[providerLocationKey(ssh)].probes

    expect(availableAgents(settings, { probes: localProbes })).toEqual(["claude"])
    expect(availableAgents(settings, { probes: sshProbes })).toEqual(["codex"])

    // Cached calls do not hit IPC again unless invalidated or forced.
    await useProviders.getState().probeLocation(local, settings.providers)
    await useProviders.getState().probeLocation(ssh, settings.providers)
    expect(ipc.providerProbe).toHaveBeenCalledTimes(3)
    expect(ipc.hostCheck).toHaveBeenCalledTimes(1)

    // Invalidating SSH causes the next probe to re-query hostCheck.
    useProviders.getState().invalidate(ssh)
    await useProviders.getState().probeLocation(ssh, settings.providers)
    expect(ipc.hostCheck).toHaveBeenCalledTimes(2)
  })

  it("re-probes a local agent when its configured binary changes", async () => {
    const settings = makeSettings()
    vi.mocked(ipc.providerProbe).mockImplementation(async (kind, bin) => probe(kind === "codex" && bin === "/opt/codex"))

    await useProviders.getState().probeLocal(settings.providers)
    expect(useProviders.getState().byLocation.local.probes.codex?.found).toBe(false)
    expect(ipc.providerProbe).toHaveBeenCalledTimes(3)

    const updated = makeSettings({ codex: { binary: "/opt/codex" } })
    await useProviders.getState().probeLocal(updated.providers)
    expect(useProviders.getState().byLocation.local.probes.codex?.found).toBe(true)
    expect(ipc.providerProbe).toHaveBeenCalledTimes(4)
  })

  it("records hostCheck results and handles remote check failures without emptying probes", async () => {
    const settings = makeSettings()
    useProviders.getState().recordHostCheck(ssh, hostCheck({ claude: false, codex: false, antigravity: true }))
    const recorded = useProviders.getState().byLocation[providerLocationKey(ssh)]
    expect(availableAgents(settings, { probes: recorded.probes })).toEqual(["antigravity"])

    vi.mocked(ipc.hostCheck).mockResolvedValueOnce({
      ...hostCheck({}),
      ok: false,
      error: "Connection refused",
    })
    await useProviders.getState().probeRemote(ssh, true)
    const failed = useProviders.getState().byLocation[providerLocationKey(ssh)]
    expect(failed.error).toBe("Connection refused")
    expect(availableAgents(settings, { probes: failed.probes })).toEqual(["claude", "codex", "antigravity"])
  })

  it("converts tool info into Probe objects cleanly", () => {
    expect(toolToProbe(tool("/usr/bin/codex", "codex 0.42.1 (abc)", null))).toEqual({
      found: true,
      path: "/usr/bin/codex",
      version: "0.42.1",
      error: null,
      outdated: null,
    })
    expect(toolToProbe(tool(null, null, "C:\\codex.exe"))).toEqual({
      found: false,
      path: null,
      version: null,
      error: "Windows copy only (C:\\codex.exe)",
      outdated: null,
    })
  })
})
