import { describe, expect, it } from "vitest"
import { filterCommands, insertSkill, menuCommands, ownCommand, parseSlash, skillCommands, skillQuery, slashQuery } from "./commands"

const agent = [
  { name: "compact", description: "theirs", hint: "<instructions>", skill: false },
  { name: "review", description: "Review", hint: "", skill: false },
  { name: "deploy", description: "Ship", hint: "", skill: false },
  { name: "pdf", description: "PDF skill", hint: "", skill: true },
  { name: "code-review", description: "Review skill", hint: "", skill: true },
  { name: "pdf", description: "duplicate PDF skill", hint: "", skill: true },
]

const antigravityAgent = [
  { name: "goal", description: "Run until the goal is finished.", hint: "", skill: false },
  { name: "plan", description: "Plan the steps before executing a task.", hint: "", skill: false },
  { name: "compact", description: "Summarize conversation history to free up the context window.", hint: "", skill: false },
  { name: "swarm", description: "Run a swarm of agents working together on a task.", hint: "", skill: false },
  { name: "model", description: "agent model", hint: "", skill: false },
  { name: "clear", description: "agent clear", hint: "", skill: false },
]

describe("slash commands", () => {
  it("lets Claude handle /compact natively while keeping RPC commands Moonfish-owned", () => {
    const claudeListed = [
      { name: "model", description: "agent model", hint: "", skill: false },
      { name: "clear", description: "agent clear", hint: "", skill: false },
      { name: "plan", description: "agent plan", hint: "", skill: false },
      ...agent,
    ]
    const menu = menuCommands("claude", claudeListed, true)
    expect(menu.map((c) => c.name)).toEqual(["goal", "model", "plan", "clear", "compact", "review", "deploy", "pdf", "code-review"])
    expect(menu.find((c) => c.name === "model")?.own).toBe("model")
    expect(menu.find((c) => c.name === "clear")?.own).toBe("clear")
    expect(menu.find((c) => c.name === "plan")?.own).toBe("plan")
    expect(menu.find((c) => c.name === "goal")?.own).toBe("goal")
    expect(menu.find((c) => c.name === "compact")).toEqual({ name: "compact", description: "theirs", hint: "<instructions>", skill: false })
    expect(ownCommand("/compact focus on API", true, "claude")).toBeNull()
    expect(ownCommand("/plan refactor", true, "claude")).toEqual({ own: "plan", args: "refactor" })
    expect(ownCommand("/goal fix tests", true, "claude")).toEqual({ own: "goal", args: "fix tests" })
    expect(ownCommand("/model opus", true, "claude")).toEqual({ own: "model", args: "opus" })
    expect(ownCommand("/clear", true, "claude")).toEqual({ own: "clear", args: "" })
  })

  it("lets Antigravity run /goal, /plan, /compact, and /swarm natively while keeping /model and /clear", () => {
    const menu = menuCommands("antigravity", antigravityAgent, true)
    expect(menu.map((c) => c.name)).toEqual(["model", "clear", "goal", "plan", "compact", "swarm"])
    expect(menu.find((c) => c.name === "model")?.own).toBe("model")
    expect(menu.find((c) => c.name === "clear")?.own).toBe("clear")
    for (const native of ["goal", "plan", "compact", "swarm"]) {
      expect(menu.find((c) => c.name === native)?.own).toBeUndefined()
      expect(ownCommand(`/${native} do the thing`, true, "antigravity")).toBeNull()
      expect(ownCommand(`/${native} do the thing`, false, "antigravity")).toBeNull()
    }
    expect(ownCommand("/model gemini", false, "antigravity")).toEqual({ own: "model", args: "gemini" })
    expect(ownCommand("/clear", true, "antigravity")).toEqual({ own: "clear", args: "" })
    expect(ownCommand("/clear", false, "antigravity")).toBeNull()

    const fallbackNew = menuCommands("antigravity", [], false)
    expect(fallbackNew.map((c) => c.name)).toContain("swarm")
    expect(fallbackNew.map((c) => c.name)).toContain("goal")
    expect(fallbackNew.map((c) => c.name)).toContain("plan")
    expect(fallbackNew.map((c) => c.name)).not.toContain("compact")
    expect(fallbackNew.find((c) => c.name === "goal")?.own).toBeUndefined()
    expect(fallbackNew.find((c) => c.name === "plan")?.own).toBeUndefined()
  })

  it("keeps all five RPC commands Moonfish-owned on Codex", () => {
    const menu = menuCommands("codex", agent, true)
    expect(menu.map((c) => c.name)).toEqual(["goal", "model", "plan", "compact", "clear", "review", "deploy", "pdf", "code-review"])
    for (const own of ["goal", "model", "plan", "compact", "clear"] as const) {
      expect(menu.find((c) => c.name === own)?.own).toBe(own)
      expect(ownCommand(`/${own} arg`, true, "codex")).toEqual({ own, args: "arg" })
    }
    expect(menu.find((c) => c.name === "compact")?.hint).toBe("")
  })

  it("leaves chat-only commands out of a new chat and falls back before the agent lists any", () => {
    const names = menuCommands("claude", [], false).map((c) => c.name)
    expect(names).toEqual(["goal", "model", "plan", "init", "review"])
  })

  it("shows /compact <instructions> hint only for Claude", () => {
    expect(menuCommands("claude", [], true).find((c) => c.name === "compact")?.hint).toBe("<instructions>")
    expect(menuCommands("codex", [], true).find((c) => c.name === "compact")?.hint).toBe("")
    expect(menuCommands("antigravity", [], true).find((c) => c.name === "compact")?.hint).toBe("")
  })

  it("keeps menuCommands and ownCommand 100% consistent without duplicates across all agents", () => {
    const overlapping = [
      { name: "goal", description: "g", hint: "", skill: false },
      { name: "model", description: "m", hint: "", skill: false },
      { name: "plan", description: "p", hint: "", skill: false },
      { name: "compact", description: "c", hint: "", skill: false },
      { name: "clear", description: "cl", hint: "", skill: false },
      { name: "swarm", description: "s", hint: "", skill: false },
    ]
    for (const kind of ["claude", "codex", "antigravity"] as const) {
      for (const inChat of [false, true]) {
        const menu = menuCommands(kind, overlapping, inChat)
        const names = menu.map((c) => c.name)
        expect(new Set(names).size).toBe(names.length)
        for (const c of menu) {
          const parsed = ownCommand(`/${c.name}`, inChat, kind)
          if (c.own) expect(parsed).toEqual({ own: c.own, args: "" })
          else expect(parsed).toBeNull()
        }
      }
    }
  })

  it("opens only at the start of the prompt", () => {
    expect(slashQuery("/", 1)).toBe("")
    expect(slashQuery("/rev", 4)).toBe("rev")
    expect(slashQuery("/review now", 11)).toBeNull()
    expect(slashQuery("see /rev", 8)).toBeNull()
  })

  it("triggers $ skill autocomplete at start or after whitespace and inserts $name at caret", () => {
    expect(skillQuery("$", 1)).toEqual({ start: 0, query: "" })
    expect(skillQuery("$pd", 3)).toEqual({ start: 0, query: "pd" })
    expect(skillQuery("please use $rev", 15)).toEqual({ start: 11, query: "rev" })
    expect(skillQuery("line1\n$pdf", 10)).toEqual({ start: 6, query: "pdf" })
    expect(skillQuery("use $p please", 6)).toEqual({ start: 4, query: "p" })
    expect(skillQuery("use $pdf", 6)).toBeNull()
    expect(skillQuery("foo$bar", 7)).toBeNull()
    expect(skillQuery("no trigger", 10)).toBeNull()

    const skills = skillCommands(agent)
    expect(skills.map((s) => s.name)).toEqual(["pdf", "code-review"])
    expect(filterCommands(skills, "rev").map((s) => s.name)).toEqual(["code-review"])

    expect(insertSkill("$p", { start: 0, query: "p" }, "pdf")).toEqual({ text: "$pdf ", caret: 5 })
    expect(insertSkill("use $p please", { start: 4, query: "p" }, "pdf")).toEqual({ text: "use $pdf please", caret: 9 })
    expect(insertSkill("check $", { start: 6, query: "" }, "code-review")).toEqual({ text: "check $code-review ", caret: 19 })
  })

  it("matches prefixes first, skills last", () => {
    const all = menuCommands("claude", agent, true)
    expect(filterCommands(all, "p").map((c) => c.name)).toEqual(["plan", "pdf"])
    expect(filterCommands(all, "rev").map((c) => c.name)).toEqual(["review", "code-review"])
    expect(filterCommands(all, "").length).toBe(all.length)
  })

  it("parses commands and finds Moonfish's own", () => {
    expect(parseSlash("/compact keep the API notes")).toEqual({ name: "compact", args: "keep the API notes" })
    expect(parseSlash("/ a")).toBeNull()
    expect(parseSlash("a/b")).toBeNull()
    expect(ownCommand("/model opus", false)).toEqual({ own: "model", args: "opus" })
    expect(ownCommand("/plan", false)).toEqual({ own: "plan", args: "" })
    expect(ownCommand("/plan refactor the auth layer", false)).toEqual({ own: "plan", args: "refactor the auth layer" })
    expect(ownCommand("/plan\nstep 1\nstep 2", true)).toEqual({ own: "plan", args: "step 1\nstep 2" })
    expect(ownCommand("/clear", false)).toBeNull()
    expect(ownCommand("/clear", true)).toEqual({ own: "clear", args: "" })
    expect(ownCommand("/review", true)).toBeNull()
    for (const kind of ["claude", "codex"] as const) {
      expect(ownCommand("/goal pause", true, kind)).toEqual({ own: "goal", args: "pause" })
      expect(menuCommands(kind, [], false)[0].name).toBe("goal")
    }
    expect(ownCommand("/goal pause", true, "antigravity")).toBeNull()
  })
})

import { emptyTranscript, reduce } from "./chat"

describe("old chat logs", () => {
  it("replays model lists saved before effort levels", () => {
    const old = { type: "models", models: [{ id: "haiku", name: "Haiku", description: "" }], current: "haiku" } as unknown as Parameters<typeof reduce>[1]
    const t = reduce(emptyTranscript(), old)
    expect(t.models[0].efforts).toEqual([])
    expect(t.models[0].defaultEffort).toBeNull()
  })
})
