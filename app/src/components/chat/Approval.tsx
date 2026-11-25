import { useEffect, useRef } from "react"
import { FilePen, ListChecks, ShieldQuestion, SquareTerminal } from "lucide-react"
import type { ApprovalRequest, Decision } from "@/lib/chat"
import { useRelative } from "./cwd"
import { useTouch } from "./touch"
import { DiffLines } from "./DiffView"
import { Markdown } from "./Markdown"
import { CARD_ARM_MS, cardKeyScope } from "@/lib/card-keys"
import { cn } from "@/lib/utils"

/** The agent's question, docked above the composer: Allow (Enter), Always (A), Deny (Esc). */
export function ApprovalCard({ request, queued = 0, onAnswer, className }: { request: ApprovalRequest; queued?: number; onAnswer: (d: Decision) => void; className?: string }) {
  const card = useRef<HTMLDivElement>(null)
  const shownAt = useRef(0)
  useEffect(() => {
    shownAt.current = performance.now()
  }, [request.requestId])
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Only when nothing else has the key: not a field, a focused button, a menu or a terminal.
      if (cardKeyScope(e, card.current) !== "card" || performance.now() - shownAt.current < CARD_ARM_MS) return
      if (e.key === "Enter") onAnswer("allow")
      else if (e.key === "Escape") onAnswer("deny")
      else if ((e.key === "a" || e.key === "A") && request.canRemember && !(e.target instanceof HTMLTextAreaElement)) onAnswer("allowAlways")
      else return
      e.preventDefault()
    }
    // Capture: the card answers before the composer sees the key (Esc there would stop the turn).
    window.addEventListener("keydown", onKey, true)
    return () => window.removeEventListener("keydown", onKey, true)
  }, [request, onAnswer])

  const rel = useRelative()
  const touch = useTouch()
  const plan = request.kind === "plan"
  const Icon = request.kind === "command" ? SquareTerminal : request.kind === "edit" ? FilePen : request.kind === "plan" ? ListChecks : ShieldQuestion
  return (
    <div ref={card} className={cn("flex min-h-0 max-h-[45vh] flex-col overflow-hidden rounded-2xl border border-input bg-card", className)}>
      <div className="min-h-0 overflow-y-auto overscroll-contain [overflow-wrap:anywhere]" role="region" aria-label="Approval details" tabIndex={0}>
        <div className={cn("flex gap-2.5 px-3.5 pt-3", touch ? "items-center gap-3" : "items-start")}>
          {touch ? (
            <span className="grid size-10 shrink-0 place-items-center self-start rounded-full bg-muted">
              <Icon className="size-[18px]" />
            </span>
          ) : (
            <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          )}
          <div className="min-w-0 flex-1">
            <div className={cn("flex items-baseline gap-2 font-semibold", touch ? "text-[14px]" : "text-[13px]")}>
              {rel(request.title)}
              {queued ? <span className="rounded-full border px-1.5 text-[10.5px] font-normal text-muted-foreground">+{queued} more waiting</span> : null}
            </div>
            {request.detail && request.kind === "plan" ? (
              <Markdown text={request.detail} className={cn("rounded-md mt-2 overflow-y-auto border bg-background px-3 py-2", touch ? "max-h-[30vh]" : "max-h-[45vh]")} />
            ) : request.detail ? (
              <div className={request.kind === "command" ? cn("mt-1.5 border bg-background px-2.5 py-1.5 font-mono text-[12px] break-all text-code", touch ? "rounded-[14px]" : "rounded-md") : "mt-0.5 text-[12.5px] text-muted-foreground"}>{rel(request.detail)}</div>
            ) : null}
          </div>
        </div>
        {request.diff ? <DiffLines patch={request.diff.patch} className={cn("mx-3.5 mt-2.5 border bg-background py-1", touch ? "max-h-[25vh] rounded-[14px]" : "rounded-md max-h-56")} /> : null}
      </div>
      {touch ? (
        // A phone: the way out and the answer side by side (the answer under the thumb), the
        // lasting one under them.
        <div className="flex shrink-0 flex-col gap-1 p-3">
          <div className="flex gap-2">
            <button className={cn("h-12 flex-1 rounded-full border border-input px-3 text-[15px] active:bg-accent", plan ? "text-foreground" : "text-red")} onClick={() => onAnswer("deny")}>
              {plan ? "Keep planning" : "Deny"}
            </button>
            <button className="h-12 flex-[1.5] rounded-full bg-primary px-3 text-[15px] font-semibold text-primary-foreground active:bg-primary/90" onClick={() => onAnswer("allow")}>
              {plan ? "Approve plan" : "Allow"}
            </button>
          </div>
          {request.canRemember ? (
            <button className="h-10 rounded-full px-3 text-[13px] text-muted-foreground active:bg-accent" onClick={() => onAnswer("allowAlways")}>
              Always allow in this chat
            </button>
          ) : null}
        </div>
      ) : (
      <div className="flex shrink-0 flex-wrap items-center gap-1.5 p-3">
        <button className="rounded-md flex h-7 items-center gap-2 bg-primary px-3 text-[12.5px] font-medium text-primary-foreground hover:bg-primary/90" onClick={() => onAnswer("allow")}>
          {plan ? "Approve plan" : "Allow"} <kbd className="font-mono text-[10.5px] opacity-60">Enter</kbd>
        </button>
        {request.canRemember ? (
          <button className="rounded-md flex h-7 items-center gap-2 border border-input px-3 text-[12.5px] hover:bg-accent" onClick={() => onAnswer("allowAlways")}>
            Always allow in this chat <kbd className="font-mono text-[10.5px] text-faint">A</kbd>
          </button>
        ) : null}
        <button className={cn("rounded-md ml-auto flex h-7 items-center gap-2 px-3 text-[12.5px] text-muted-foreground hover:bg-accent ", plan ? "hover:text-foreground" : "hover:text-red")} onClick={() => onAnswer("deny")}>
          {plan ? "Keep planning" : "Deny"} <kbd className="font-mono text-[10.5px] text-faint">Esc</kbd>
        </button>
      </div>
      )}
    </div>
  )
}
