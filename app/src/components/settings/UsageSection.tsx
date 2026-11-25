import { useEffect } from "react"
import { RefreshCw } from "lucide-react"
import { ProviderLimits } from "@/components/ProviderLimits"
import { Button } from "@/components/ui/button"
import { formatTokens } from "@/lib/chat"
import { useChats } from "@/store/chats"
import { LOCAL_USAGE, useAllUsage } from "@/store/usage"
import { useLocationAwake } from "@/store/wsl"
import { useApp, useProject } from "@/store/app"
import { Group, Section } from "./layout"

export function UsageSection() {
  const projects = useApp((s) => s.settings!.projects)
  const metas = useChats((s) => s.metas)
  const activeLocation = useProject()?.location ?? LOCAL_USAGE
  const awake = useLocationAwake(activeLocation)
  const { accounts, pendingAgents, locations, loading, load: fetchLimits } = useAllUsage(activeLocation)
  const load = () => void fetchLimits(true)

  useEffect(() => {
    void fetchLimits(true)
  }, [fetchLimits, awake])

  const hasAny = accounts.length > 0 || pendingAgents.length > 0
  const multiLocation = locations.length > 1 || accounts.some((a) => a.locations.some((l) => l !== "This computer"))

  return (
    <Section
      title="Usage"
      description={
        multiLocation
          ? "Plan limits grouped by account across this computer and connected environments."
          : "Plan limits for this computer."
      }
      actions={
        <Button variant="outline" size="sm" onClick={load} disabled={loading}>
          <RefreshCw className={loading ? "animate-spin" : undefined} /> Refresh
        </Button>
      }
    >
      {!hasAny ? (
        <p className="text-[12.5px] text-muted-foreground">No agent with plan limits is installed and turned on.</p>
      ) : null}
      {accounts.map((account, idx) => (
        <ProviderLimits
          key={`${account.agent}:${account.email ?? idx}:${account.locations.join(",")}`}
          agent={account.agent}
          limits={account.limits}
          email={account.email}
          plan={account.plan}
          locations={multiLocation ? account.locations : undefined}
        />
      ))}
      {pendingAgents.map((agent) => (
        <ProviderLimits key={`pending:${agent}`} agent={agent} limits={undefined} />
      ))}
      <Group title="By project">
        <div className="grid grid-cols-[1fr_repeat(3,7rem)] border-b bg-muted/50 px-4 py-2 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
          <span>Project</span>
          <span className="text-right">Chats</span>
          <span className="text-right">Tokens</span>
          <span className="text-right">Cost</span>
        </div>
        {projects.length === 0 ? (
          <div className="px-4 py-3 text-[12px] text-faint">No projects yet.</div>
        ) : (
          projects.map((p) => {
            const mine = metas.filter((m) => m.projectId === p.id)
            const tokens = mine.reduce((n, m) => n + m.tokens, 0)
            const cost = mine.reduce((n, m) => n + m.costUsd, 0)
            return (
              <div key={p.id} className="grid grid-cols-[1fr_repeat(3,7rem)] px-4 py-2 font-mono text-[12px]">
                <span className="truncate font-sans">{p.name}</span>
                <span className="text-right text-muted-foreground">{mine.length}</span>
                <span className="text-right text-muted-foreground">{formatTokens(tokens)}</span>
                <span className="text-right text-muted-foreground">${cost.toFixed(2)}</span>
              </div>
            )
          })
        )}
      </Group>
      <p className="-mt-2 text-[11.5px] text-faint">Cost is what Claude Code reports per turn; Codex doesn't report cost.</p>
    </Section>
  )
}
