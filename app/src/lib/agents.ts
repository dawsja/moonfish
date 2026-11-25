// The agents a chat can use.

import type { ListChoice } from "./provider-list"
import { AGENTS, type AgentKind, type Location, type Probe, type Settings } from "./types"

/** Agents turned on in Settings -> Providers. */
export function agentChoices(settings: Settings): AgentKind[] {
  return AGENTS.filter((k) => settings.providers[k].enabled)
}

export type AvailableAgentsOptions = {
  /** User's explicit provider list choices ("shown" | "hidden") from Settings -> Providers. */
  choices?: Partial<Record<AgentKind, ListChoice>>
  /** CLI probe results for the target location; missing entries mean still probing or unknown. */
  probes?: Partial<Record<AgentKind, Pick<Probe, "found">>>
  /** Currently selected agent to keep visible in the picker if it is already selected. */
  selected?: AgentKind | null
}

/** Agents to show in the chat Agent picker for a target project location.
 *  - Never includes providers disabled in settings (`enabled === false`).
 *  - Never includes providers explicitly hidden (`choice === "hidden"`), unless fallback needs one.
 *  - Always includes providers explicitly added (`choice === "shown"`), unless disabled.
 *  - Otherwise includes providers installed at the target location (or still probing).
 *  - Falls back to enabled providers if all would be filtered out, and always keeps `selected`. */
export function availableAgents(settings: Settings, options: AvailableAgentsOptions = {}): AgentKind[] {
  const { choices = {}, probes = {}, selected = null } = options
  const enabled = agentChoices(settings)

  const filtered = enabled.filter((a) => {
    const choice = choices[a]
    if (choice === "hidden") return false
    if (choice === "shown") return true
    const probe = probes[a]
    // Unknown / still probing: keep visible until probes settle so the menu never flickers empty.
    if (!probe) return true
    return probe.found
  })

  let result: AgentKind[]
  if (filtered.length > 0) {
    result = filtered
  } else {
    const nonHidden = enabled.filter((a) => choices[a] !== "hidden")
    result = nonHidden.length > 0 ? nonHidden : enabled
  }

  if (selected && !result.includes(selected)) {
    const withSelected = new Set<AgentKind>([...result, selected])
    return AGENTS.filter((a) => withSelected.has(a))
  }

  return result
}

/** Picks the active agent for a new chat: the user's pick if still available, else the preferred
 *  default if available, else the first available agent. */
export function pickNewChatAgent(preferred: AgentKind, available: AgentKind[], picked?: AgentKind | null): AgentKind {
  if (picked && available.includes(picked)) return picked
  if (available.includes(preferred)) return preferred
  return available[0] ?? preferred
}

export type MobileAgentLike = {
  kind: AgentKind
  enabled?: boolean
  available?: boolean
  installed?: boolean
}

/** Filters desktop-reported agents for mobile's new-chat picker while staying compatible with
 *  desktops that omit optional availability fields. */
export function mobileAvailableAgents<T extends MobileAgentLike>(agents: T[], location?: Location): T[] {
  const enabled = agents.filter((a) => a.enabled !== false)
  const isLocal = !location || location.kind === "local"
  const filtered = enabled.filter((a) => {
    if (a.available === false) return false
    if (a.available === true) return true
    if (isLocal && a.installed === false) return false
    return true
  })
  return filtered.length > 0 ? filtered : enabled
}
