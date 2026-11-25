import { useState } from "react"
import { Check, ChevronDown, ChevronRight, Circle, LoaderCircle } from "lucide-react"
import type { PlanItem } from "@/lib/chat"
import { cn } from "@/lib/utils"

/** The agent's checklist, folded to its current step. */
export function PlanDock({ items, className }: { items: PlanItem[]; className?: string }) {
  const [open, setOpen] = useState(false)
  const done = items.filter((i) => i.status === "completed").length
  const current = items.find((i) => i.status === "inProgress") ?? items.find((i) => i.status === "pending")
  return (
    <div className={cn("overflow-hidden rounded-2xl border bg-card text-[12.5px]", className)}>
      <button
        className="flex h-8 w-full items-center gap-2 px-4 text-left hover:bg-accent focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring pointer-coarse:h-10"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {open ? <ChevronDown className="size-3.5 shrink-0 text-faint" /> : <ChevronRight className="size-3.5 shrink-0 text-faint" />}
        <span className="font-medium">Plan</span>
        <span className="shrink-0 font-mono text-[11px] text-faint" aria-label={`${done} of ${items.length} steps complete`}>
          {done}/{items.length}
        </span>
        {!open && current ? <span className="truncate text-muted-foreground">{current.text}</span> : null}
      </button>
      {open ? (
        <ul className="border-t px-3 py-2">
          {items.map((it, i) => (
            <li key={i} className="flex items-center gap-2 py-0.5">
              {it.status === "completed" ? (
                <Check className="size-3.5 shrink-0 text-green" />
              ) : it.status === "inProgress" ? (
                <LoaderCircle className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
              ) : (
                <Circle className="size-3.5 shrink-0 text-faint" />
              )}
              <span className={cn("min-w-0 break-words", it.status === "completed" && "text-muted-foreground line-through")}>{it.text}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}
