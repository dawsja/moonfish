// Plan limits across local, WSL, and SSH locations, grouped and deduplicated by account.

import { useCallback, useMemo, useRef } from "react"
import { create } from "zustand"
import { ipc } from "@/lib/ipc"
import { AGENTS, type AccountUsage, type AgentKind, type LimitWindow, type Limits, type Location, type Settings } from "@/lib/types"
import { useApp } from "./app"
import { useWsl } from "./wsl"

export type UsageEntry = {
  limits: Partial<Record<AgentKind, Limits>>
  /** Per agent, unix ms of its last answer without an error. Only those are reused. */
  okAt: Partial<Record<AgentKind, number>>
  loading: boolean
  /** Unix ms of the last fetch. */
  at: number | null
}

type UsageStore = {
  entries: Record<string, UsageEntry>
  load(location: Location, force?: boolean): Promise<void>
  loadAll(locations: Location[], force?: boolean): Promise<void>
  invalidate(location?: Location, agent?: AgentKind): void
}

export const LOCAL_USAGE: Location = { kind: "local" }
const EMPTY: UsageEntry = { limits: {}, okAt: {}, loading: false, at: null }

export function usageLocationKey(location: Location): string {
  return location.kind === "local" ? "local" : JSON.stringify([location.kind, location.kind === "wsl" ? location.distro : location.host])
}

export function usageLocationLabel(location: Location): string {
  return location.kind === "local" ? "This computer" : location.kind === "wsl" ? `WSL · ${location.distro}` : `SSH · ${location.host}`
}

const STALE = 5 * 60_000

/** An answer is reused for five minutes; a failed one (not signed in, no key, rejected,
 *  offline) is asked again next time, so signing in shows without a manual refresh. */
export const useUsage = create<UsageStore>((set, get) => ({
  entries: {},
  async load(location, force = false) {
    const key = usageLocationKey(location)
    const { loading, okAt } = get().entries[key] ?? EMPTY
    if (loading) return
    const now = Date.now()
    const due = limitAgents(useApp.getState().settings).filter((a) => force || !(now - (okAt[a] ?? -Infinity) < STALE))
    if (!due.length) return
    const update = (edit: (entry: UsageEntry) => UsageEntry) => set((s) => ({ entries: { ...s.entries, [key]: edit(s.entries[key] ?? EMPTY) } }))
    const answer = (a: AgentKind, l: Limits) =>
      update((entry) => {
        const { [a]: _, ...rest } = entry.okAt
        return { ...entry, limits: { ...entry.limits, [a]: l }, okAt: l.error ? rest : { ...rest, [a]: Date.now() } }
      })
    update((entry) => ({ ...entry, loading: true }))
    await Promise.all(
      due.map((a) =>
        ipc.providerLimits(a, location).then(
          (l) => answer(a, l),
          (e) => answer(a, { windows: [], error: String(e), extra: [] }),
        ),
      ),
    )
    update((entry) => ({ ...entry, loading: false, at: Date.now() }))
  },
  async loadAll(locations, force = false) {
    const seen = new Set<string>()
    const unique: Location[] = []
    for (const loc of locations) {
      const key = usageLocationKey(loc)
      if (!seen.has(key)) {
        seen.add(key)
        unique.push(loc)
      }
    }
    await Promise.all(unique.map((loc) => get().load(loc, force)))
  },
  invalidate(location, agent) {
    set((s) => {
      if (!location) {
        const next: Record<string, UsageEntry> = {}
        for (const [k, entry] of Object.entries(s.entries)) {
          if (!agent) {
            next[k] = { ...entry, okAt: {} }
          } else {
            const { [agent]: _, ...rest } = entry.okAt
            next[k] = { ...entry, okAt: rest }
          }
        }
        return { entries: next }
      }
      const key = usageLocationKey(location)
      const entry = s.entries[key]
      if (!entry) return s
      if (!agent) return { entries: { ...s.entries, [key]: { ...entry, okAt: {} } } }
      const { [agent]: _, ...rest } = entry.okAt
      return { entries: { ...s.entries, [key]: { ...entry, okAt: rest } } }
    })
  },
}))

/** The agents whose limits are worth asking for: those on in Settings -> Providers. */
export function limitAgents(settings: Settings | null): AgentKind[] {
  return AGENTS.filter((a) => settings?.providers[a]?.enabled !== false)
}

/** The agents with limits to show at a location: on in Settings, and installed there (an
 *  uninstalled CLI's leftover sign-in isn't worth a row). */
export function shownAgents(settings: Settings | null, limits: Partial<Record<AgentKind, Limits>>): AgentKind[] {
  return limitAgents(settings).filter((a) => !limits[a]?.missing)
}

/** Unique locations to query for usage: active project first, then local, configured SSH hosts,
 *  and project WSL/SSH locations. Stopped WSL distros are skipped when `runningWsl` is supplied. */
export function usageLocations(
  settings: Settings | null,
  activeLocation?: Location | null,
  runningWsl?: Record<string, boolean>,
): Location[] {
  const out: Location[] = []
  const seen = new Set<string>()
  const add = (loc: Location | null | undefined) => {
    if (!loc) return
    if (loc.kind === "wsl" && runningWsl && runningWsl[loc.distro] !== true) return
    const key = usageLocationKey(loc)
    if (seen.has(key)) return
    seen.add(key)
    out.push(loc)
  }
  add(activeLocation)
  add(LOCAL_USAGE)
  for (const h of settings?.sshHosts ?? []) {
    if (h.host.trim()) add({ kind: "ssh", host: h.host.trim() })
  }
  for (const p of settings?.projects ?? []) {
    add(p.location)
  }
  return out
}

export type LocationLimits = {
  location: string
  limits: Partial<Record<AgentKind, Limits>>
}

function normalizeEmail(email: string | null | undefined): string | null {
  const trimmed = email?.trim()
  return trimmed ? trimmed.toLowerCase() : null
}

/** Groups and deduplicates plan limits across locations by `(agent, normalizedEmail)`:
 *  - Same email across multiple locations collapses into 1 row with combined `locations`.
 *  - Distinct emails for the same agent stay separate rows.
 *  - Any healthy non-error result for an agent suppresses `error` and `missing` rows from other locations. */
export function dedupeUsageByAccount(sources: LocationLimits[], agents: AgentKind[] = AGENTS): AccountUsage[] {
  const out: AccountUsage[] = []

  for (const agent of agents) {
    const present = sources
      .map((s) => ({ location: s.location, limits: s.limits[agent] }))
      .filter((r): r is { location: string; limits: Limits } => r.limits !== undefined && !r.limits.missing)

    if (!present.length) continue

    const healthy = present.filter((r) => !r.limits.error)
    const rows = healthy.length > 0 ? healthy : present
    const groups = new Map<string, AccountUsage>()

    rows.forEach((row, index) => {
      const norm = normalizeEmail(row.limits.accountEmail)
      const key = norm ? `email:${norm}` : healthy.length > 0 ? `anon:${row.location}:${index}` : "error"
      const existing = groups.get(key)
      if (!existing) {
        groups.set(key, {
          agent,
          email: row.limits.accountEmail?.trim() || null,
          plan: row.limits.accountPlan?.trim() || null,
          locations: [row.location],
          limits: row.limits,
        })
        return
      }
      if (!existing.locations.includes(row.location)) {
        existing.locations.push(row.location)
      }
      if (!existing.plan && row.limits.accountPlan?.trim()) {
        existing.plan = row.limits.accountPlan.trim()
      }
      if (existing.limits.windows.length === 0 && row.limits.windows.length > 0) {
        existing.limits = row.limits
      }
    })

    out.push(...groups.values())
  }

  return out
}

export function useUsageAt(location: Location) {
  const key = usageLocationKey(location)
  const entry = useUsage((s) => s.entries[key] ?? EMPTY)
  const fetch = useUsage((s) => s.load)
  // Keyed by where, not by the location object: every settings edit makes a new one, and a
  // refetch is a CLI start and an HTTP call per agent.
  const at = useRef(location)
  at.current = location
  const load = useCallback((force = false) => fetch(at.current, force), [fetch, key])
  return { ...entry, load }
}

/** Account-grouped, deduplicated usage across local and configured remote locations. */
export function useAllUsage(activeLocation?: Location | null) {
  const settings = useApp((s) => s.settings)
  const runningWsl = useWsl((s) => s.running)
  const entries = useUsage((s) => s.entries)
  const loadAll = useUsage((s) => s.loadAll)

  const locations = useMemo(
    () => usageLocations(settings, activeLocation, runningWsl),
    [settings, activeLocation, runningWsl],
  )
  const locationsKey = locations.map(usageLocationKey).join("|")
  const locRef = useRef(locations)
  locRef.current = locations

  const load = useCallback((force = false) => loadAll(locRef.current, force), [loadAll, locationsKey])

  const enabled = useMemo(() => limitAgents(settings), [settings])
  const loading = locations.some((l) => entries[usageLocationKey(l)]?.loading)

  const accounts = useMemo(() => {
    const sources: LocationLimits[] = locations.map((l) => ({
      location: usageLocationLabel(l),
      limits: entries[usageLocationKey(l)]?.limits ?? {},
    }))
    return dedupeUsageByAccount(sources, enabled)
  }, [locations, entries, enabled])

  const pendingAgents = useMemo(() => {
    const represented = new Set(accounts.map((a) => a.agent))
    return enabled.filter((agent) => {
      if (represented.has(agent)) return false
      return locations.some((l) => {
        const entry = entries[usageLocationKey(l)]
        return entry?.limits[agent] === undefined
      })
    })
  }, [accounts, enabled, locations, entries])

  return { accounts, pendingAgents, locations, loading, load }
}

/** A window's chip label: `5h`, `wk`, `30d`, or the model a weekly limit is for. A window for a
 *  group of models ("Gemini · 5 hours", Antigravity's) keeps the group: `Gemini 5h`. */
export function shortLabel(w: LimitWindow): string {
  const group = /^(.+?) · (.+)$/.exec(w.label)
  if (group) return `${group[1]} ${shortLabel({ ...w, label: group[2] })}`
  const l = w.label.toLowerCase()
  const model = /this week, (?!all)(.+)$/i.exec(w.label)
  if (model) return model[1]
  if (l.includes("5 hour") || l.includes("session")) return "5h"
  if (l.includes("week")) return "wk"
  if (l.includes("month")) return "30d"
  const days = /(\d+) day/.exec(l)
  if (days) return `${days[1]}d`
  const hours = /(\d+) hour/.exec(l)
  return hours ? `${hours[1]}h` : w.label
}

/** "in 2d 17h", "in 4h 47m", "in 12m". */
export function resetsIn(ms: number | null, now = Date.now()): string | null {
  if (!ms) return null
  const m = Math.max(0, Math.round((ms - now) / 60_000))
  const d = Math.floor(m / 1440)
  const h = Math.floor((m % 1440) / 60)
  if (d) return `in ${d}d ${h}h`
  if (h) return `in ${h}h ${m % 60}m`
  return `in ${m}m`
}

/** The window that resets first: what "Resets in …" on an agent's row means. */
export function nextReset(l: Limits | undefined): number | null {
  const times = (l?.windows ?? []).map((w) => w.resetsAt).filter((t): t is number => !!t)
  return times.length ? Math.min(...times) : null
}
