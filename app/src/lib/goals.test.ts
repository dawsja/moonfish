import { describe, expect, it } from "vitest"
import { hideGoalMarkers, readGoalCommand, supportsGoals } from "./goals"

describe("goal commands", () => {
  it("enables Moonfish goals only for agents without a native /goal command", () => {
    expect(supportsGoals("claude")).toBe(true)
    expect(supportsGoals("codex")).toBe(true)
    expect(supportsGoals("antigravity")).toBe(false)
    expect(supportsGoals(undefined)).toBe(false)
  })

  it("separates lifecycle commands from objectives", () => {
    expect(readGoalCommand("/goal")).toEqual({ action: "view" })
    expect(readGoalCommand("/goal pause")).toEqual({ action: "pause" })
    expect(readGoalCommand("/goal RESUME")).toEqual({ action: "resume" })
    expect(readGoalCommand("/goal clear")).toEqual({ action: "clear" })
    expect(readGoalCommand("/goal edit")).toEqual({ action: "edit" })
    expect(readGoalCommand("/goal edit Run tests")).toEqual({ action: "set", objective: "Run tests" })
    expect(readGoalCommand("/goal pause the demo timer")).toEqual({ action: "set", objective: "pause the demo timer" })
    expect(readGoalCommand("/goalposts")).toBeNull()
  })

  it("keeps verification text and hides goal markers", () => {
    expect(hideGoalMarkers("Tests passed.\nMOONFISH_GOAL_COMPLETE_af312")).toBe("Tests passed.")
    expect(hideGoalMarkers("Choose a model.\nMOONFISH_GOAL_BLOCKED_e61\n")).toBe("Choose a model.\n")
  })
})
