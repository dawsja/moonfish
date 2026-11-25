import { useEffect, useState } from "react"
import { RefreshCw } from "lucide-react"
import { useShallow } from "zustand/react/shallow"
import { ProviderLimits } from "@/components/ProviderLimits"
import { formatTokens } from "@/lib/chat"
import type { AccountUsage, AgentKind } from "@/lib/types"
import { cn } from "@/lib/utils"
import { back } from "./route"
import { PullSheet, SheetHead } from "./sheet"
import { RoundButton } from "./ui"
import { api, type LimitsResponse } from "./rpc"
import { useMobile } from "./store"

type UsageCard =
  | { key: string; kind: AgentKind; loading: true }
  | { key: string; kind: AgentKind; loading: false; account?: AccountUsage; limits: LimitsResponse }

export function UsageSheet() {
  const name = useMobile((s) => s.desktopName)
  const connected = useMobile((s) => s.link === "connected")
  const listed = useMobile((s) => s.listed)
  const projects = useMobile((s) => s.projects)
  const metas = useMobile((s) => s.metas)
  const kinds = useMobile(useShallow((s) => s.agents.map((a) => a.kind)))
  const [tab, setTab] = useState<"Plan limits" | "By project">("Plan limits")
  const [revision, refresh] = useState(0)
  const [limits, setLimits] = useState<Partial<Record<AgentKind, LimitsResponse>>>({})

  useEffect(() => {
    let active = true
    setLimits({})
    if (connected && listed) {
      for (const kind of kinds) {
        void api
          .limits(kind)
          .catch((e): LimitsResponse => ({
            windows: [],
            extra: [],
            error: String(e).includes("unknown method") ? "Update Moonfish on the desktop to see plan limits here." : String(e),
          }))
          .then((result) => {
            if (active) setLimits((data) => ({ ...data, [kind]: result }))
          })
      }
    }
    return () => { active = false }
  }, [kinds, connected, listed, revision])

  const cards: UsageCard[] = []
  for (const kind of kinds) {
    const res = limits[kind]
    if (!res) {
      cards.push({ key: `loading:${kind}`, kind, loading: true })
      continue
    }
    if (Array.isArray(res.accounts) && res.accounts.length > 0) {
      res.accounts.forEach((account, idx) => {
        cards.push({
          key: `${kind}:${account.email ?? idx}:${account.locations.join(",")}`,
          kind: account.agent,
          loading: false,
          account,
          limits: account.limits,
        })
      })
      continue
    }
    if (!res.missing) {
      cards.push({ key: kind, kind, loading: false, limits: res })
    }
  }

  const loadingAny = kinds.some((k) => !limits[k])

  return (
    <PullSheet
      open
      onClose={back}
      history={false}
      title="Usage"
      description={`Paired desktop · ${name}`}
      header={
        <SheetHead
          title="Usage"
          sub={name}
          aside={
            tab === "Plan limits" && connected && kinds.length ? (
              <RoundButton label="Refresh" className="bg-muted" disabled={loadingAny} onClick={() => refresh((n) => n + 1)}>
                <RefreshCw className="size-4" />
              </RoundButton>
            ) : null
          }
        />
      }
    >
      <div className="mx-3 mb-3 flex shrink-0 gap-1 rounded-full bg-muted p-1" role="group" aria-label="Usage breakdown">
        {(["Plan limits", "By project"] as const).map((item) => (
          <button
            key={item}
            aria-pressed={tab === item}
            className={cn("h-10 flex-1 rounded-full text-[13.5px]", tab === item ? "bg-primary font-semibold text-primary-foreground" : "active:bg-accent")}
            onClick={() => setTab(item)}
          >
            {item}
          </button>
        ))}
      </div>
      <div className="min-h-0 overflow-y-auto px-3">
        {!connected ? <p role="status" className="mb-3 text-sm text-muted-foreground">Reconnect to {name} to update usage.</p> : null}
        {tab === "Plan limits" ? (
          <>
            {connected && !listed ? <p role="status">Loading providers...</p> : null}
            {connected && listed && !cards.length ? <p className="text-sm text-muted-foreground">No providers with plan limits on this desktop.</p> : null}
            {connected && listed ? (
              <div className="space-y-2">
                {cards.map((card) =>
                  card.loading ? (
                    <ProviderLimits key={card.key} agent={card.kind} limits={undefined} />
                  ) : (
                    <ProviderLimits
                      key={card.key}
                      agent={card.kind}
                      limits={card.limits}
                      email={card.account?.email}
                      plan={card.account?.plan}
                      locations={card.account?.locations}
                    />
                  ),
                )}
              </div>
            ) : null}
          </>
        ) : !listed ? (
          <p role="status" className="py-3 text-sm text-muted-foreground">Waiting for project totals from the desktop...</p>
        ) : (
          <>
            <table className="w-full text-left text-[13px] tabular-nums">
              <thead className="border-b text-xs text-muted-foreground">
                <tr>
                  <th className="py-2">Project</th>
                  <th className="text-right">Chats</th>
                  <th className="text-right">Tokens</th>
                  <th className="text-right">Cost</th>
                </tr>
              </thead>
              <tbody>
                {projects.map((p) => {
                  const chats = metas.filter((m) => m.projectId === p.id)
                  const cost = chats.reduce((n, c) => n + c.costUsd, 0)
                  const tokens = chats.reduce((n, c) => n + c.tokens, 0)
                  const reportsCost = cost > 0 || chats.some((c) => c.agent === "claude")
                  return (
                    <tr key={p.id} className="border-b">
                      <th scope="row" className="max-w-24 truncate py-3 font-medium">{p.name}</th>
                      <td className="text-right">{chats.length}</td>
                      <td className="text-right">{formatTokens(tokens)}</td>
                      <td className="text-right">{reportsCost ? `$${cost.toFixed(2)}` : "-"}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
            {!projects.length && <p className="py-3 text-sm text-muted-foreground">No projects yet.</p>}
            <p className="mt-3 text-xs text-muted-foreground">Cost includes amounts reported by Claude Code.</p>
          </>
        )}
      </div>
    </PullSheet>
  )
}
