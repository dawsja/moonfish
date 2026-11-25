import { useEffect } from "react"
import { ChevronRight, Gauge, RefreshCw } from "lucide-react"
import { AgentGlyph } from "@/components/icons"
import { Tip } from "@/components/Tip"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { LIMIT_LABEL, type AccountUsage, type AgentKind } from "@/lib/types"
import { cn } from "@/lib/utils"
import { useApp, useProject } from "@/store/app"
import { LOCAL_USAGE, nextReset, resetsIn, shortLabel, useAllUsage } from "@/store/usage"
import { useLocationAwake } from "@/store/wsl"

/** The title bar's usage icon and a popover with account-grouped usage across locations. */
export function UsageButton() {
  const activeLocation = useProject()?.location ?? LOCAL_USAGE
  const awake = useLocationAwake(activeLocation)
  const { accounts, pendingAgents, locations, loading, load } = useAllUsage(activeLocation)

  useEffect(() => {
    void load(true)
    const t = setInterval(() => document.visibilityState === "visible" && void load(), 5 * 60_000)
    return () => clearInterval(t)
  }, [load, awake])

  const top = Math.max(0, ...accounts.flatMap((a) => a.limits.windows.map((w) => w.usedPercent)))
  const tone = top >= 90 ? "text-red" : top >= 70 ? "text-warn" : "text-muted-foreground"
  const hasAny = accounts.length > 0 || pendingAgents.length > 0
  const showLocations = locations.length > 1 || accounts.some((a) => a.locations.some((l) => l !== "This computer"))

  return (
    <Popover onOpenChange={(o) => o && void load()}>
      <Tip content="Usage" side="bottom">
        <PopoverTrigger asChild>
          <button className="rounded-md flex h-7 items-center px-2 text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Usage">
            <Gauge className={cn("size-3.5", tone)} />
          </button>
        </PopoverTrigger>
      </Tip>
      <PopoverContent align="end" className="w-80 overflow-hidden p-0">
        <div className="flex items-center px-3 pt-2.5 pb-1.5 text-[12.5px] font-semibold">
          <span className="min-w-0 truncate">Usage</span>
          <button className="rounded-md ml-auto grid size-6 place-items-center text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Refresh usage" onClick={() => void load(true)}>
            <RefreshCw className={cn("size-3.5", loading && "animate-spin")} />
          </button>
        </div>
        {accounts.map((account, idx) => (
          <AccountRow
            key={`${account.agent}:${account.email ?? idx}:${account.locations.join(",")}`}
            account={account}
            showLocations={showLocations}
          />
        ))}
        {pendingAgents.map((agent) => (
          <PendingRow key={`pending:${agent}`} agent={agent} />
        ))}
        {!hasAny ? <div className="border-t px-3 py-2.5 text-[12px] text-faint">No agent with plan limits is set up.</div> : null}
        <button
          className="flex w-full items-center border-t px-3 py-2.5 text-left text-[12.5px] hover:bg-accent"
          onClick={() => useApp.getState().openSettings("usage")}
        >
          Usage details
          <ChevronRight className="ml-auto size-3.5 text-muted-foreground" />
        </button>
      </PopoverContent>
    </Popover>
  )
}

function PendingRow({ agent }: { agent: AgentKind }) {
  return (
    <div className="border-t px-3 py-2.5">
      <div className="flex items-center gap-2 text-[12.5px]">
        <AgentGlyph agent={agent} className="size-4" />
        <span className="font-medium">{LIMIT_LABEL[agent]}</span>
      </div>
      <div className="mt-1.5 h-3 w-40 animate-pulse rounded-full bg-muted" />
    </div>
  )
}

function AccountRow({ account, showLocations }: { account: AccountUsage; showLocations: boolean }) {
  const { agent, email, plan, locations, limits } = account
  const reset = resetsIn(nextReset(limits))
  const subtitleParts: string[] = []
  if (email) subtitleParts.push(email)
  if (showLocations && locations.length) subtitleParts.push(locations.join(" · "))

  return (
    <div className="border-t px-3 py-2.5">
      <div className="flex items-center gap-1.5 text-[12.5px]">
        <AgentGlyph agent={agent} className="size-4 shrink-0" />
        <span className="font-medium">{LIMIT_LABEL[agent]}</span>
        {plan ? (
          <span className="rounded-full border px-1.5 py-px font-mono text-[9.5px] uppercase text-muted-foreground">
            {plan}
          </span>
        ) : null}
        {reset ? <span className="ml-auto shrink-0 text-[11px] text-faint">Resets {reset}</span> : null}
      </div>
      {subtitleParts.length ? (
        <div className="mt-0.5 truncate text-[11px] text-muted-foreground" title={subtitleParts.join(" · ")}>
          {subtitleParts.join(" · ")}
        </div>
      ) : null}
      {limits.error ? (
        <div className="mt-1 text-[11.5px] leading-snug text-faint">{limits.error}</div>
      ) : (
        <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1">
          {limits.windows.map((w) => (
            <span key={w.label} className="flex items-center gap-1.5 text-[11px]" title={w.label}>
              <span className="text-faint">{shortLabel(w)}</span>
              <span className="flex h-1.5 w-10 overflow-hidden rounded-full bg-muted">
                <span className={cn("h-full", w.usedPercent >= 90 ? "bg-red" : w.usedPercent >= 70 ? "bg-warn" : "bg-primary")} style={{ width: `${Math.min(100, w.usedPercent)}%` }} />
              </span>
              <span className="font-mono text-muted-foreground">{Math.round(w.usedPercent)}%</span>
            </span>
          ))}
        </div>
      )}
    </div>
  )
}
