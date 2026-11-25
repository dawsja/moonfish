import { useEffect, useRef, useState } from "react"
import type { ModelOption, SlashCommand } from "@/lib/chat"
import type { AgentInfo, Catalog } from "./rpc"
import { useMobile } from "./store"

const lastListedCommandsByAgent = new Map<string, SlashCommand[]>()

/** Models and commands for a chat: what its agent said, else the agent's catalog (loaded when asked). */
export function useCatalog(
  agent: AgentInfo | undefined,
  projectId: string | null,
  live: { models: ModelOption[]; commands: SlashCommand[] } = { models: [], commands: [] },
) {
  const key = agent ? `${agent.kind}|${projectId ?? ""}` : ""
  const [catalog, setCatalog] = useState<Catalog | null>(null)
  const [failed, setFailed] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const request = useRef(0)
  useEffect(() => {
    setCatalog(null)
    setFailed(false)
    setError(null)
    return () => { request.current++ }
  }, [agent?.kind, projectId])
  if (key && live.commands.length) lastListedCommandsByAgent.set(key, live.commands)
  const checkSpeed = agent?.kind === "claude" || agent?.kind === "codex"
  const load = async () => {
    if (!agent || (!checkSpeed && live.models.length && live.commands.length)) return
    const id = ++request.current
    if (checkSpeed || !catalog?.models.length) setCatalog(null)
    setFailed(false)
    setError(null)
    const result = await useMobile.getState().catalog(agent, projectId)
    if (id !== request.current) return
    if (result.catalog) {
      setCatalog(result.catalog)
      if (!result.catalog.models.length) setFailed(true)
    } else {
      setFailed(true)
      setError(result.error)
    }
  }
  const models = checkSpeed && catalog?.models.length ? catalog.models : live.models.length ? live.models : (catalog?.models ?? [])
  const commands = live.commands.length ? live.commands : (lastListedCommandsByAgent.get(key) ?? catalog?.commands ?? [])
  return { models, commands, load, failed: !live.models.length && failed, error: !live.models.length ? error : null }
}

