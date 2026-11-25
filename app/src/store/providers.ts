// Shared reactive state for provider list choices (shown/hidden) and CLI availability probes
// per location (local via providerProbe, SSH/WSL via hostCheck).

import { create } from "zustand"
import { ipc } from "@/lib/ipc"
import { loadChoices, saveChoices, type ListChoice } from "@/lib/provider-list"
import { AGENTS, type AgentKind, type HostCheck, type Location, type Probe, type Settings, type Tool } from "@/lib/types"
import { useApp } from "./app"

export type LocationProbes = {
  /** Resolved CLI probe per agent; absent until probed. */
  probes: Partial<Record<AgentKind, Probe>>
  /** Local binary string each local agent was probed with, so editing Executable re-probes. */
  probedBinary: Partial<Record<AgentKind, string>>
  /** Per-agent in-flight local probe flags. */
  checking: Partial<Record<AgentKind, boolean>>
  /** True while a remote hostCheck is in flight. */
  checkingAll: boolean
  /** Error from the last remote hostCheck, if it failed. */
  error: string | null
  /** Unix ms when this location's probe last completed, or null if never completed. */
  at: number | null
}

export const EMPTY_LOCATION_PROBES: LocationProbes = {
  probes: {},
  probedBinary: {},
  checking: {},
  checkingAll: false,
  error: null,
  at: null,
}

export function providerLocationKey(location: Location): string {
  return location.kind === "local" ? "local" : JSON.stringify([location.kind, location.kind === "wsl" ? location.distro : location.host])
}

export function toolToProbe(tool: Tool): Probe {
  const version = tool.version?.match(/\d+\.\d+(\.\d+)?/)?.[0] ?? tool.version
  return {
    found: !!tool.path,
    path: tool.path,
    version,
    error: tool.windows ? `Windows copy only (${tool.windows})` : null,
    outdated: null,
  }
}

type ProvidersStore = {
  choices: Partial<Record<AgentKind, ListChoice>>
  byLocation: Record<string, LocationProbes>

  setChoice(kind: AgentKind, choice: ListChoice): void
  reloadChoices(): void
  probeLocalAgent(kind: AgentKind, binary?: string): Promise<Probe | null>
  probeLocal(providers?: Settings["providers"], force?: boolean): Promise<void>
  probeRemote(location: Location, force?: boolean): Promise<HostCheck | null>
  probeLocation(location: Location, providers?: Settings["providers"], force?: boolean): Promise<void>
  recordHostCheck(location: Location, check: HostCheck): void
  invalidate(location?: Location): void
}

const localSeq: Record<AgentKind, number> = { claude: 0, codex: 0, antigravity: 0 }
const localInFlightBinary: Partial<Record<AgentKind, string>> = {}
const remoteSeq = new Map<string, number>()

export const useProviders = create<ProvidersStore>((set, get) => ({
  choices: loadChoices(),
  byLocation: {},

  setChoice(kind, choice) {
    const next = { ...get().choices, [kind]: choice }
    set({ choices: next })
    saveChoices(next)
  },

  reloadChoices() {
    set({ choices: loadChoices() })
  },

  async probeLocalAgent(kind, binary) {
    const resolvedBinary = binary ?? useApp.getState().settings?.providers[kind].binary ?? ""
    const seq = ++localSeq[kind]
    localInFlightBinary[kind] = resolvedBinary
    set((s) => {
      const entry = s.byLocation.local ?? EMPTY_LOCATION_PROBES
      return {
        byLocation: {
          ...s.byLocation,
          local: { ...entry, checking: { ...entry.checking, [kind]: true }, error: null },
        },
      }
    })
    try {
      const probe = await ipc.providerProbe(kind, resolvedBinary)
      if (localSeq[kind] !== seq) return null
      set((s) => {
        const entry = s.byLocation.local ?? EMPTY_LOCATION_PROBES
        return {
          byLocation: {
            ...s.byLocation,
            local: {
              ...entry,
              probes: { ...entry.probes, [kind]: probe },
              probedBinary: { ...entry.probedBinary, [kind]: resolvedBinary },
              checking: { ...entry.checking, [kind]: false },
              error: null,
              at: Date.now(),
            },
          },
        }
      })
      return probe
    } catch {
      if (localSeq[kind] === seq) {
        set((s) => {
          const entry = s.byLocation.local ?? EMPTY_LOCATION_PROBES
          return {
            byLocation: {
              ...s.byLocation,
              local: { ...entry, checking: { ...entry.checking, [kind]: false } },
            },
          }
        })
      }
      return null
    } finally {
      if (localSeq[kind] === seq) delete localInFlightBinary[kind]
    }
  },

  async probeLocal(providers, force = false) {
    const resolved = providers ?? useApp.getState().settings?.providers
    const entry = get().byLocation.local ?? EMPTY_LOCATION_PROBES
    const due = AGENTS.filter((kind) => {
      const bin = resolved?.[kind].binary ?? ""
      if (force) return true
      if (entry.probes[kind] !== undefined && entry.probedBinary[kind] === bin) return false
      if (entry.checking[kind] && localInFlightBinary[kind] === bin) return false
      return true
    })
    if (!due.length) return
    await Promise.all(due.map((kind) => get().probeLocalAgent(kind, resolved?.[kind].binary ?? "")))
  },

  async probeRemote(location, force = false) {
    if (location.kind === "local") return null
    const key = providerLocationKey(location)
    const existing = get().byLocation[key]
    if (!force && existing && (existing.checkingAll || existing.at !== null)) return null

    const seq = (remoteSeq.get(key) ?? 0) + 1
    remoteSeq.set(key, seq)

    set((s) => {
      const entry = s.byLocation[key] ?? EMPTY_LOCATION_PROBES
      return {
        byLocation: {
          ...s.byLocation,
          [key]: { ...entry, checkingAll: true, error: null },
        },
      }
    })

    const result = await ipc.hostCheck(location).catch((e) => ({ ok: false, error: String(e) }) as HostCheck)
    if (remoteSeq.get(key) !== seq) return null

    set((s) => ({
      byLocation: {
        ...s.byLocation,
        [key]: hostCheckToEntry(result),
      },
    }))
    return result
  },

  async probeLocation(location, providers, force = false) {
    if (location.kind === "local") {
      await get().probeLocal(providers, force)
    } else {
      await get().probeRemote(location, force)
    }
  },

  recordHostCheck(location, check) {
    if (location.kind === "local") return
    const key = providerLocationKey(location)
    remoteSeq.set(key, (remoteSeq.get(key) ?? 0) + 1)
    set((s) => ({
      byLocation: {
        ...s.byLocation,
        [key]: hostCheckToEntry(check),
      },
    }))
  },

  invalidate(location) {
    if (!location) {
      for (const a of AGENTS) {
        localSeq[a] += 1
        delete localInFlightBinary[a]
      }
      for (const key of remoteSeq.keys()) remoteSeq.set(key, (remoteSeq.get(key) ?? 0) + 1)
      set({ byLocation: {} })
      return
    }
    const key = providerLocationKey(location)
    if (location.kind === "local") {
      for (const a of AGENTS) {
        localSeq[a] += 1
        delete localInFlightBinary[a]
      }
    } else {
      remoteSeq.set(key, (remoteSeq.get(key) ?? 0) + 1)
    }
    set((s) => {
      if (!(key in s.byLocation)) return s
      const next = { ...s.byLocation }
      delete next[key]
      return { byLocation: next }
    })
  },
}))

function hostCheckToEntry(result: HostCheck): LocationProbes {
  if (!result.ok) {
    return {
      ...EMPTY_LOCATION_PROBES,
      checkingAll: false,
      error: result.error ?? "Could not connect.",
      at: Date.now(),
    }
  }
  return {
    probes: {
      claude: toolToProbe(result.claude),
      codex: toolToProbe(result.codex),
      antigravity: toolToProbe(result.antigravity),
    },
    probedBinary: {},
    checking: {},
    checkingAll: false,
    error: null,
    at: Date.now(),
  }
}

export function useLocationProbes(location: Location): LocationProbes {
  const key = providerLocationKey(location)
  return useProviders((s) => s.byLocation[key] ?? EMPTY_LOCATION_PROBES)
}
