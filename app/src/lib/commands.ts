import { supportsGoals } from "./goals"
import type { SlashCommand } from "./chat"
import type { AgentKind } from "./types"

/** Commands Moonfish runs itself instead of sending them to the agent. */
export type OwnCommand = "model" | "plan" | "clear" | "compact" | "goal"

export type MenuCommand = SlashCommand & { own?: OwnCommand }

const OWN: (MenuCommand & { chatOnly?: boolean })[] = [
  { name: "goal", description: "Start a goal or view its status", hint: "[objective, pause, resume, clear]", skill: false, own: "goal" },
  { name: "model", description: "Choose a model or enter its ID", hint: "<model>", skill: false, own: "model" },
  { name: "plan", description: "Ask the agent for a plan before it makes changes", hint: "[prompt]", skill: false, own: "plan" },
  { name: "compact", description: "Summarize the conversation so far to free up context", hint: "", skill: false, own: "compact", chatOnly: true },
  { name: "clear", description: "Clear the agent's context and keep the chat transcript", hint: "", skill: false, own: "clear", chatOnly: true },
]

/** Commands Moonfish runs via protocol RPCs rather than forwarding as prompt text. */
function ownsCommand(kind: AgentKind | undefined, own: OwnCommand): boolean {
  switch (own) {
    case "model":
    case "clear":
      return true
    case "goal":
      return supportsGoals(kind)
    case "plan":
      return kind !== "antigravity"
    case "compact":
      return kind === "codex"
  }
}

/** Before an agent lists its own (a chat that hasn't started, or an older CLI). */
const FALLBACK: Partial<Record<AgentKind, (SlashCommand & { chatOnly?: boolean })[]>> = {
  claude: [
    { name: "compact", description: "Summarize the conversation so far to free up context", hint: "<instructions>", skill: false, chatOnly: true },
    { name: "init", description: "Initialize a new CLAUDE.md file with codebase documentation", hint: "", skill: false },
    { name: "review", description: "Review the current changes", hint: "", skill: false },
  ],
  antigravity: [
    { name: "goal", description: "Run until the goal is finished.", hint: "", skill: false },
    { name: "compact", description: "Summarize conversation history to free up the context window.", hint: "", skill: false, chatOnly: true },
    { name: "schedule", description: "Run an instruction on a recurring schedule or as a one-time timer.", hint: "", skill: false },
    { name: "browser", description: "Run a browser agent for web tasks.", hint: "", skill: false },
    { name: "plan", description: "Plan the steps before executing a task.", hint: "", skill: false },
    { name: "grill-me", description: "Interview me to align on a plan.", hint: "", skill: false },
    { name: "teamwork", description: "Run a team of agents on large projects.", hint: "", skill: false },
    { name: "swarm", description: "Run a swarm of agents working together on a task.", hint: "", skill: false },
    { name: "learn", description: "Reflect on recent successes or corrections to capture reusable skills or rules.", hint: "", skill: false },
    { name: "boost", description: "Run the Boost multi-agent orchestrator for complex tasks.", hint: "", skill: false },
  ],
}

/** What the menu offers: Moonfish's commands, then the agent's (minus names Moonfish handles). */
export function menuCommands(kind: AgentKind, agent: SlashCommand[], inChat: boolean): MenuCommand[] {
  const own = OWN.filter((c) => ownsCommand(kind, c.own!) && (inChat || !c.chatOnly)).map(({ chatOnly: _, ...c }) => c)
  const taken = new Set(OWN.filter((c) => ownsCommand(kind, c.own!)).map((c) => c.name))
  const listed = agent.length
    ? agent
    : (FALLBACK[kind] ?? []).filter((c) => inChat || !c.chatOnly).map(({ chatOnly: _, ...c }) => c)
  const seen = new Set<string>()
  const theirs = listed.filter((c) => {
    const key = c.name.toLowerCase()
    return !taken.has(key) && !seen.has(key) && seen.add(key)
  })
  return [...own, ...theirs]
}

/** Deduped skills the agent offers (for `$` skill autocomplete). */
export function skillCommands(agent: SlashCommand[]): MenuCommand[] {
  const seen = new Set<string>()
  return agent.filter((c) => c.skill && !seen.has(c.name) && seen.add(c.name))
}

/** `/na` at the very start of the prompt, with nothing after it yet: the query `na`. */
export function slashQuery(text: string, caret: number): string | null {
  const m = /^\/(\S*)$/.exec(text.slice(0, caret))
  return m && !/\s/.test(text.slice(caret, caret + 1) || "") ? m[1] : null
}

/** The `$query` skill mention being typed right before the caret, at start or after whitespace. */
export function skillQuery(text: string, caret: number): { start: number; query: string } | null {
  const m = /(?:^|\s)\$(\S*)$/.exec(text.slice(0, caret))
  if (!m || /\S/.test(text.slice(caret, caret + 1) || "")) return null
  return { start: caret - m[1].length - 1, query: m[1] }
}

/** Replaces the `$query` at `trigger` with `$name ` and returns the new text and caret. */
export function insertSkill(text: string, trigger: { start: number; query: string }, name: string): { text: string; caret: number } {
  const end = trigger.start + 1 + trigger.query.length
  const after = text.slice(end)
  const insert = `$${name} `
  return {
    text: text.slice(0, trigger.start) + insert + (after.startsWith(" ") ? after.slice(1) : after),
    caret: trigger.start + insert.length,
  }
}

/** Names starting with the query first, then names with a word that does (`code-review`,
 *  `cloudflare:wrangler`); skills after commands. */
export function filterCommands(all: MenuCommand[], query: string): MenuCommand[] {
  const q = query.toLowerCase()
  const score = (c: MenuCommand) => {
    const n = c.name.toLowerCase()
    return n.startsWith(q) ? 0 : n.split(/[-_:.]/).some((w) => w.startsWith(q)) ? 1 : -1
  }
  return all
    .map((c, i) => ({ c, i, s: score(c) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => a.s - b.s || Number(a.c.skill) - Number(b.c.skill) || a.i - b.i)
    .map((x) => x.c)
}

/** `/compact keep the API notes` → `{ name: "compact", args: "keep the API notes" }`. */
export function parseSlash(text: string): { name: string; args: string } | null {
  const m = /^\/([^\s/]+)(?:\s+([\s\S]*))?$/.exec(text.trim())
  return m ? { name: m[1], args: (m[2] ?? "").trim() } : null
}

/** The Moonfish command a prompt runs, if any. */
export function ownCommand(text: string, inChat: boolean, kind?: AgentKind): { own: OwnCommand; args: string } | null {
  const p = parseSlash(text)
  const c = p && OWN.find((o) => o.name === p.name.toLowerCase() && ownsCommand(kind, o.own!) && (inChat || !o.chatOnly))
  return c && p ? { own: c.own!, args: p.args } : null
}
