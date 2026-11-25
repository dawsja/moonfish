import type { ToolItem, Transcript } from "./chat"

export type SubagentStatus = "running" | "completed" | "failed" | "stopped"

export type SubagentItem = {
  id: string
  taskId?: string
  itemId?: string
  title: string
  role?: string
  prompt?: string
  status: SubagentStatus
  detail?: string
  output?: string
  startedAt?: number
  endedAt?: number
  stoppable: boolean
  tracked: boolean
  background: boolean
  sessionId?: string
  interactive: boolean
}

const ROLE_KEYS = ["Role", "role", "subagent_type", "subagentType", "TypeName", "typeName", "agent_type", "agentType", "name", "Name"] as const
const PROMPT_KEYS = ["Prompt", "prompt", "task", "Task", "brief", "Brief", "description", "Description", "instructions", "Instructions"] as const

function asRecord(input: unknown): Record<string, unknown> | null {
  if (!input) return null
  if (typeof input === "object" && !Array.isArray(input)) return input as Record<string, unknown>
  if (typeof input === "string") {
    const trimmed = input.trim()
    if (trimmed.startsWith("{")) {
      try {
        const parsed: unknown = JSON.parse(trimmed)
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) return parsed as Record<string, unknown>
      } catch {
        return null
      }
    }
  }
  return null
}

function pickString(record: Record<string, unknown>, keys: readonly string[]): string | undefined {
  for (const key of keys) {
    const value = record[key]
    if (typeof value === "string") {
      const trimmed = value.trim()
      if (trimmed) return trimmed
    }
  }
  return undefined
}

function extractMeta(input: unknown): { role?: string; prompt?: string } {
  const record = asRecord(input)
  if (record) {
    return {
      role: pickString(record, ROLE_KEYS),
      prompt: pickString(record, PROMPT_KEYS),
    }
  }
  if (typeof input === "string") {
    const trimmed = input.trim()
    if (trimmed) return { prompt: trimmed }
  }
  return {}
}

function firstLine(text: string | undefined | null): string | undefined {
  if (!text) return undefined
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (trimmed) return trimmed
  }
  return undefined
}

function toolStatus(status: ToolItem["status"]): SubagentStatus {
  if (status === "running") return "running"
  if (status === "ok") return "completed"
  return "failed"
}

type RankedSubagent = {
  item: SubagentItem
  order: number
}

/** Extracts subagents from a transcript by merging live/recorded agent tasks with task tool calls.
 *  Running subagents are listed first in start order, followed by finished subagents most recent first. */
export function extractSubagents(transcript: Transcript): SubagentItem[] {
  const toolsById = new Map<string, { item: ToolItem; index: number }>()
  transcript.items.forEach((item, index) => {
    if (item.kind === "tool") toolsById.set(item.id, { item, index })
  })

  const linkedItemIds = new Set<string>()
  const ranked: RankedSubagent[] = []
  let seq = 0

  for (const task of transcript.tasks) {
    if (task.kind !== "agent") continue
    const linked = task.itemId ? toolsById.get(task.itemId) : undefined
    if (task.itemId) linkedItemIds.add(task.itemId)

    const tool = linked?.item
    const { role, prompt } = extractMeta(tool?.tool.input)
    const title = firstLine(task.title) ?? firstLine(tool?.tool.title) ?? firstLine(prompt) ?? role ?? "Subagent"
    const detail = task.detail?.trim() || undefined
    const output = tool?.output.trim() || undefined

    ranked.push({
      item: {
        id: task.id,
        taskId: task.id,
        itemId: task.itemId ?? undefined,
        title,
        role,
        prompt,
        status: task.status,
        detail,
        output,
        startedAt: task.startedAt,
        endedAt: task.endedAt ?? undefined,
        stoppable: task.stoppable,
        tracked: task.tracked,
        background: task.background ?? true,
        sessionId: task.sessionId ?? undefined,
        interactive: !!task.interactive,
      },
      order: linked ? linked.index : transcript.items.length + seq,
    })
    seq++
  }

  transcript.items.forEach((item, index) => {
    if (item.kind !== "tool" || item.tool.kind !== "task") return
    if (linkedItemIds.has(item.id)) return

    const { role, prompt } = extractMeta(item.tool.input)
    const title = firstLine(item.tool.title) ?? firstLine(prompt) ?? role ?? "Subagent"
    const output = item.output.trim() || undefined

    ranked.push({
      item: {
        id: item.id,
        itemId: item.id,
        title,
        role,
        prompt,
        status: toolStatus(item.status),
        output,
        stoppable: false,
        tracked: true,
        background: false,
        interactive: false,
      },
      order: index,
    })
  })

  ranked.sort((a, b) => {
    const aRunning = a.item.status === "running"
    const bRunning = b.item.status === "running"
    if (aRunning !== bRunning) return aRunning ? -1 : 1

    if (aRunning) {
      const aStart = a.item.startedAt
      const bStart = b.item.startedAt
      if (aStart !== undefined && bStart !== undefined && aStart !== bStart) return aStart - bStart
      return a.order - b.order
    }

    const aTime = a.item.endedAt ?? a.item.startedAt
    const bTime = b.item.endedAt ?? b.item.startedAt
    if (aTime !== undefined && bTime !== undefined && aTime !== bTime) return bTime - aTime
    return b.order - a.order
  })

  return ranked.map((r) => r.item)
}

/** Concise summary string such as "2 running · 1 done" or "1 failed · 2 done". */
export function subagentSummary(subagents: SubagentItem[]): string {
  const running = subagents.filter((s) => s.status === "running").length
  const failed = subagents.filter((s) => s.status === "failed").length
  const stopped = subagents.filter((s) => s.status === "stopped").length
  const done = subagents.filter((s) => s.status === "completed").length
  return [
    running ? `${running} running` : "",
    done ? `${done} done` : "",
    failed ? `${failed} failed` : "",
    stopped ? `${stopped} stopped` : "",
  ]
    .filter(Boolean)
    .join(" · ")
}
