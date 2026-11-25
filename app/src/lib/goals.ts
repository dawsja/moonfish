import type { AgentKind } from "./types"

export type Goal = {
  objective: string
  status: "active" | "paused" | "blocked" | "usageLimited" | "budgetLimited" | "complete"
  tokensUsed?: number
  tokenBudget?: number
  note?: string
}

export type GoalAction = { action: "set"; objective: string } | { action: "pause" | "resume" | "clear" }
export type GoalInput = GoalAction | { action: "view" | "edit" }

export const supportsGoals = (agent: AgentKind | undefined) => agent === "claude" || agent === "codex"

export function readGoalCommand(text: string): GoalInput | null {
  const command = /^\/goal(?:\s+([\s\S]*))?$/i.exec(text.trim())
  if (!command) return null
  const args = command[1]?.trim() ?? ""
  switch (args.toLowerCase()) {
    case "":
    case "status":
    case "help":
      return { action: "view" }
    case "edit":
      return { action: "edit" }
    case "pause":
    case "stop":
      return { action: "pause" }
    case "resume":
      return { action: "resume" }
    case "clear":
    case "cancel":
      return { action: "clear" }
    default:
      return { action: "set", objective: args.replace(/^edit\s+/i, "") }
  }
}

export function hideGoalMarkers(text: string): string {
  return text.replace(/(?:^|\n)MOONFISH_GOAL_(?:COMPLETE|BLOCKED)_[a-f\d]*[ \t]*(?=\r?$|\n)/gm, "")
}
