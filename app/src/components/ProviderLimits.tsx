import { AgentGlyph } from "@/components/icons"
import { LIMIT_LABEL, type AgentKind, type Limits } from "@/lib/types"
import { Group } from "./settings/layout"
import { useTouch } from "./chat/touch"

function resetTime(ms: number | null): string | undefined {
  if (!ms) return undefined
  const d = new Date(ms)
  const sameDay = d.toDateString() === new Date().toDateString()
  return sameDay ? `at ${d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}` : d.toLocaleDateString(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" })
}

/** One plan limit: a square bar, how much is used, and when it resets. */
function LimitBar({ label, used, resets, track = "bg-muted" }: { label: string; used: number | null; resets?: string; track?: string }) {
  const pct = used === null ? 0 : Math.min(100, Math.max(0, used))
  const tone = pct >= 90 ? "bg-red" : pct >= 70 ? "bg-warn" : "bg-primary"
  return (
    <div className="px-4 py-3">
      <div className="mb-1.5 flex items-baseline text-[12.5px]">
        <span className="font-medium">{label}</span>
        <span className="ml-auto font-mono text-[11.5px] text-muted-foreground">{used === null ? "Unavailable" : `${Math.round(pct)}% used`}</span>
      </div>
      <div className="flex h-2 gap-px">
        {Array.from({ length: 20 }, (_, i) => (
          <span key={i} className={`flex-1 ${i < Math.round(pct / 5) ? tone : track}`} />
        ))}
      </div>
      {resets ? <div className="mt-1 text-[11px] text-faint">Resets {resets}</div> : null}
    </div>
  )
}

/** A provider's plan limits. On the phone: a round card, its name as a heading (not a label). */
export function ProviderLimits({
  agent,
  limits,
  email,
  plan,
  locations,
}: {
  agent: AgentKind
  limits?: Limits
  email?: string | null
  plan?: string | null
  locations?: string[]
}) {
  const touch = useTouch()
  const track = touch ? "bg-background" : "bg-muted"
  const accountEmail = email ?? limits?.accountEmail ?? null
  const accountPlan = plan ?? limits?.accountPlan ?? null
  const locationTags = locations?.filter(Boolean) ?? []

  const rows = (
    <>
      {limits?.error ? <div className="px-4 py-3 text-[12px] text-muted-foreground">{limits.error}</div> : null}
      {limits && !limits.error && limits.windows.length === 0 ? <div className="px-4 py-3 text-[12px] text-faint">No plan limits reported.</div> : null}
      {limits === undefined ? <LimitBar label="..." used={null} track={track} /> : limits?.windows.map((w) => <LimitBar key={w.label} label={w.label} used={w.usedPercent} resets={resetTime(w.resetsAt)} track={track} />)}
      {limits && !limits.error
        ? limits.extra.map((x) => (
            <div key={x.label} className="border-t px-4 py-3 text-[12.5px]">
              <div className="flex items-baseline">
                <span className="font-medium">{x.label}</span>
                <span className="ml-auto font-mono text-[11.5px] text-muted-foreground">{x.value ?? "Unavailable"}</span>
              </div>
              {x.note ? <div className="mt-0.5 text-[11px] text-faint">{x.note}</div> : null}
            </div>
          ))
        : null}
    </>
  )
  if (touch)
    return (
      <section className="rounded-[24px] bg-muted pb-1">
        <div className="px-4 pt-3.5">
          <div className="flex items-center gap-2 text-[14px] font-semibold">
            <AgentGlyph agent={agent} className="size-4" />
            <span>{LIMIT_LABEL[agent]}</span>
            {accountPlan ? (
              <span className="rounded-full border border-border/80 bg-background/60 px-1.5 py-px font-mono text-[10px] font-normal uppercase text-muted-foreground">
                {accountPlan}
              </span>
            ) : null}
            {limits === undefined ? <span className="ml-auto text-[11.5px] font-normal text-faint">Loading...</span> : null}
          </div>
          {accountEmail || locationTags.length ? (
            <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11.5px] text-muted-foreground">
              {accountEmail ? <span className="truncate">{accountEmail}</span> : null}
              {locationTags.length ? <span className="text-faint">{locationTags.join(" · ")}</span> : null}
            </div>
          ) : null}
        </div>
        {rows}
      </section>
    )
  return (
    <Group
      title={
        <span className="flex flex-wrap items-center gap-1.5">
          <AgentGlyph agent={agent} className="size-3" />
          <span>{LIMIT_LABEL[agent]}</span>
          {accountEmail ? <span className="font-normal normal-case tracking-normal text-foreground">{accountEmail}</span> : null}
          {accountPlan ? (
            <span className="rounded-full border px-1.5 py-px font-mono text-[10px] font-normal uppercase text-muted-foreground">
              {accountPlan}
            </span>
          ) : null}
        </span>
      }
      aside={
        limits === undefined ? (
          <span className="text-[11px] text-faint">Loading...</span>
        ) : locationTags.length ? (
          <div className="flex flex-wrap items-center justify-end gap-1">
            {locationTags.map((loc) => (
              <span key={loc} className="rounded-full border bg-muted/40 px-2 py-0.5 text-[10.5px] text-muted-foreground">
                {loc}
              </span>
            ))}
          </div>
        ) : null
      }
    >
      {rows}
    </Group>
  )
}
