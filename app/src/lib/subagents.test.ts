import { describe, expect, it } from "vitest"
import { emptyTranscript, type BackgroundTask, type ToolItem, type Transcript } from "./chat"
import { extractSubagents, subagentSummary } from "./subagents"

const makeTask = (overrides: Partial<BackgroundTask> = {}): BackgroundTask => ({
  id: "task-1",
  kind: "agent",
  title: "Explore auth flow",
  status: "running",
  detail: null,
  itemId: null,
  startedAt: 1_000,
  endedAt: null,
  tracked: true,
  stoppable: true,
  background: false,
  ...overrides,
})

const makeTool = (overrides: Partial<ToolItem> = {}): ToolItem => ({
  kind: "tool",
  id: "tool-1",
  tool: {
    kind: "task",
    name: "Agent",
    title: "Explore auth flow",
    input: {
      description: "Explore auth flow",
      prompt: "Inspect JWT refresh in auth/",
      subagent_type: "Explore",
    },
    path: null,
  },
  status: "running",
  output: "",
  diff: null,
  ...overrides,
})

const transcriptWith = (patch: Partial<Transcript>): Transcript => ({
  ...emptyTranscript(),
  ...patch,
})

describe("extractSubagents", () => {
  it("merges a Claude BackgroundTask with its linked ToolItem and enriches role, prompt, and output", () => {
    const t = transcriptWith({
      items: [
        makeTool({
          id: "call-1",
          status: "ok",
          output: "Found 3 token handlers in auth/login.rs",
        }),
      ],
      tasks: [
        makeTask({
          id: "agent-1",
          itemId: "call-1",
          title: "Explore auth flow",
          status: "completed",
          detail: "Finished reading login.rs",
          startedAt: 1_000,
          endedAt: 32_000,
          stoppable: false,
          background: false,
        }),
      ],
    })

    const result = extractSubagents(t)
    expect(result).toHaveLength(1)
    expect(result[0]).toEqual({
      id: "agent-1",
      taskId: "agent-1",
      itemId: "call-1",
      title: "Explore auth flow",
      role: "Explore",
      prompt: "Inspect JWT refresh in auth/",
      status: "completed",
      detail: "Finished reading login.rs",
      output: "Found 3 token handlers in auth/login.rs",
      startedAt: 1_000,
      endedAt: 32_000,
      stoppable: false,
      tracked: true,
      background: false,
      sessionId: undefined,
      interactive: false,
    })
  })

  it("ignores non-agent background tasks (commands, watchers, other)", () => {
    const t = transcriptWith({
      tasks: [
        makeTask({ id: "cmd-1", kind: "command", title: "npm run dev" }),
        makeTask({ id: "watch-1", kind: "watch", title: "CI watch" }),
        makeTask({ id: "other-1", kind: "other", title: "Misc job" }),
      ],
    })

    expect(extractSubagents(t)).toEqual([])
  })

  it("captures replayed or Codex/ACP ToolKind::Task items that have no BackgroundTask", () => {
    const t = transcriptWith({
      items: [
        makeTool({
          id: "acp-1",
          tool: {
            kind: "task",
            name: "delegate",
            title: "Audit Rust clippy",
            input: { Role: "reviewer", Prompt: "Run clippy on all crates" },
            path: null,
          },
          status: "ok",
          output: "0 warnings found",
        }),
        makeTool({
          id: "codex-1",
          tool: {
            kind: "task",
            name: "collab_agent",
            title: "",
            input: { agent_type: "worker", brief: "Check failing tests\nin moonfish-agents" },
            path: null,
          },
          status: "error",
          output: "1 test failed",
        }),
      ],
    })

    const result = extractSubagents(t)
    expect(result).toHaveLength(2)
    // Most recent finished item first (codex-1 at index 1 before acp-1 at index 0)
    expect(result[0].taskId).toBeUndefined()
    expect(result[0]).toMatchObject({
      id: "codex-1",
      itemId: "codex-1",
      title: "Check failing tests",
      role: "worker",
      prompt: "Check failing tests\nin moonfish-agents",
      status: "failed",
      output: "1 test failed",
      stoppable: false,
      tracked: true,
      background: false,
      interactive: false,
    })
    expect(result[1].taskId).toBeUndefined()
    expect(result[1]).toMatchObject({
      id: "acp-1",
      itemId: "acp-1",
      title: "Audit Rust clippy",
      role: "reviewer",
      prompt: "Run clippy on all crates",
      status: "completed",
      output: "0 warnings found",
    })
  })

  it("deduplicates by itemId while preserving multiple tasks spawned from one tool call", () => {
    const t = transcriptWith({
      items: [
        makeTool({
          id: "spawn-batch",
          tool: {
            kind: "task",
            name: "spawn_agent",
            title: "Check release workflow",
            input: { prompt: "Check release workflow", TypeName: "explorer" },
            path: null,
          },
          status: "running",
        }),
      ],
      tasks: [
        makeTask({ id: "thread-a", itemId: "spawn-batch", title: "Thread A", startedAt: 100 }),
        makeTask({ id: "thread-b", itemId: "spawn-batch", title: "Thread B", startedAt: 200 }),
      ],
    })

    const result = extractSubagents(t)
    expect(result.map((s) => s.id)).toEqual(["thread-a", "thread-b"])
    expect(result[0].role).toBe("explorer")
    expect(result[1].prompt).toBe("Check release workflow")
  })

  it("orders running subagents first by start order, then finished subagents most recent first", () => {
    const t = transcriptWith({
      tasks: [
        makeTask({ id: "done-old", title: "Done old", status: "completed", startedAt: 100, endedAt: 500 }),
        makeTask({ id: "run-new", title: "Run unit tests", status: "running", startedAt: 300 }),
        makeTask({ id: "failed-recent", title: "Lint check", status: "failed", startedAt: 200, endedAt: 900 }),
        makeTask({ id: "run-old", title: "Explore auth flow", status: "running", startedAt: 150 }),
        makeTask({ id: "stopped-mid", title: "Watch logs", status: "stopped", startedAt: 120, endedAt: 700 }),
      ],
    })

    const result = extractSubagents(t)
    expect(result.map((s) => s.id)).toEqual(["run-old", "run-new", "failed-recent", "stopped-mid", "done-old"])
  })

  it("defaults omitted background field on legacy tasks to true and parses stringified JSON input", () => {
    const legacyTask = makeTask({ id: "legacy-1", itemId: "tool-json" })
    delete legacyTask.background

    const t = transcriptWith({
      items: [
        makeTool({
          id: "tool-json",
          tool: {
            kind: "task",
            name: "Task",
            title: "Parse JSON input",
            input: JSON.stringify({ name: "verifier", task: "Verify migration" }),
            path: null,
          },
        }),
      ],
      tasks: [legacyTask],
    })

    const [sub] = extractSubagents(t)
    expect(sub.background).toBe(true)
    expect(sub.role).toBe("verifier")
    expect(sub.prompt).toBe("Verify migration")
  })
})

describe("subagentSummary", () => {
  it("formats counts concisely", () => {
    const t = transcriptWith({
      tasks: [
        makeTask({ id: "r1", status: "running" }),
        makeTask({ id: "r2", status: "running" }),
        makeTask({ id: "c1", status: "completed", endedAt: 2000 }),
        makeTask({ id: "f1", status: "failed", endedAt: 3000 }),
      ],
    })
    expect(subagentSummary(extractSubagents(t))).toBe("2 running · 1 done · 1 failed")
  })
})
